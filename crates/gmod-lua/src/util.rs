//! The parts of `util` that the engine implements and that need no world: network strings,
//! KeyValues and JSON, checksums and encodings, SteamIDs.

use std::cell::RefCell;
use std::fmt::Write as _;

use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::host;
use crate::vector::{Angle, Vector};

/// Names `util.AddNetworkString` registered, in order: the ID of a name is its position + 1.
#[derive(Default)]
pub(crate) struct NetworkStrings(pub RefCell<Vec<String>>);

/// How many nested tables JSON may hold before `util.JSONToTable` gives up, as in GMod.
const JSON_DEPTH: usize = 15;

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let util = lua.create_table()?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        util.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("AddNetworkString", |a| Ok(Value::Integer(add_network_string(a.lua, &a.string(1)?) as i64)))?;
    def("NetworkStringToID", |a| {
        let name = a.string(1)?;
        let host = host(a.lua);
        let strings = host.network_strings.0.borrow();
        Ok(Value::Integer(strings.iter().position(|s| s.eq_ignore_ascii_case(&name)).map_or(0, |i| i as i64 + 1)))
    })?;
    def("NetworkIDToString", |a| {
        let id = a.number(1)? as usize;
        let name = host(a.lua).network_strings.0.borrow().get(id.wrapping_sub(1)).cloned();
        Ok(match name {
            Some(name) => Value::String(a.lua.create_string(name)?),
            None => Value::Nil,
        })
    })?;
    def("KeyValuesToTable", |a| keyvalues_to_table(a, false))?;
    def("KeyValuesToTablePreserveOrder", |a| keyvalues_to_table(a, true))?;
    def("TableToKeyValues", |a| {
        let table = a.table(1)?;
        let root = a.opt_string(2)?.unwrap_or_else(|| "TableToKeyValues".into());
        let mut out = String::new();
        write_keyvalues(a.lua, &mut out, &root, &table, 0)?;
        Ok(Value::String(a.lua.create_string(out)?))
    })?;
    def("TableToJSON", |a| {
        let table = a.table(1)?;
        let mut out = String::new();
        write_json(a.lua, &mut out, &Value::Table(table), a.boolean(2), 0)?;
        Ok(Value::String(a.lua.create_string(out)?))
    })?;
    def("JSONToTable", |a| {
        let text = a.string(1)?;
        let limits = !a.boolean(2);
        let conversions = !a.boolean(3);
        let mut parser = Json { text: text.as_bytes(), at: 0, lua: a.lua, limits, conversions };
        Ok(match parser.document() {
            Some(Value::Table(t)) => Value::Table(t),
            _ => Value::Nil,
        })
    })?;
    def("CRC", |a| {
        let data = bytes(a, 1)?;
        Ok(Value::String(a.lua.create_string(gmod_fs::crc32::crc32(&data).to_string())?))
    })?;
    def("Base64Encode", |a| {
        let encoded = base64_encode(&bytes(a, 1)?, !a.boolean(2));
        Ok(Value::String(a.lua.create_string(encoded)?))
    })?;
    def("Base64Decode", |a| {
        Ok(match base64_decode(&a.string(1)?) {
            Some(decoded) => Value::String(a.lua.create_string(decoded)?),
            None => Value::Nil,
        })
    })?;
    def("Decompress", |a| {
        let data = bytes(a, 1)?;
        let limit = a.opt_number(2)?.map_or(u64::MAX, |n| n.max(0.0) as u64);
        Ok(match gmod_fs::lzma::decompress_alone(&data, limit) {
            Ok(out) if !data.is_empty() => Value::String(a.lua.create_string(out)?),
            _ => Value::Nil,
        })
    })?;
    def("DateStamp", |a| {
        let date: mlua::Function = a.lua.globals().get::<Table>("os")?.get("date")?;
        date.call::<Value>("%Y-%m-%d %H-%M-%S")
    })?;
    def("IsValidModel", |a| {
        let model = a.string(1)?.to_ascii_lowercase();
        let ok = model.ends_with(".mdl") && host(a.lua).vfs.exists(&model, "GAME");
        Ok(Value::Boolean(ok))
    })?;
    // a prop has a physics model of one solid, a ragdoll one of more
    def("IsValidProp", |a| {
        let model = a.string(1)?;
        Ok(Value::Boolean(crate::physics::solids(a.lua, &model) == 1))
    })?;
    def("IsValidRagdoll", |a| {
        let model = a.string(1)?;
        Ok(Value::Boolean(crate::physics::solids(a.lua, &model) > 1))
    })?;
    // decals are drawn by clients, which are not there yet
    def("Decal", |a| a.string(1).map(|_| Value::Nil))?;
    def("DecalMaterial", |a| Ok(Value::String(a.lua.create_string(a.string(1)?)?)))?;
    def("SpriteTrail", sprite_trail)?;
    // precaching is for the renderer and the sound system, which load what they need
    def("PrecacheModel", |_| Ok(Value::Nil))?;
    def("PrecacheSound", |_| Ok(Value::Nil))?;
    def("IsBinaryModuleInstalled", |_| Ok(Value::Boolean(false)))?;
    def("SteamIDTo64", |a| {
        let id = steamid_to_64(&a.string(1)?).unwrap_or(0);
        Ok(Value::String(a.lua.create_string(id.to_string())?))
    })?;
    def("SteamIDFrom64", |a| {
        let id = a.string(1)?.trim().parse::<u64>().ok().and_then(steamid_from_64).unwrap_or_else(|| "STEAM_0:0:0".into());
        Ok(Value::String(a.lua.create_string(id)?))
    })?;
    lua.globals().raw_set("util", util)?;
    lua.globals().raw_set("PrecacheParticleSystem", lua.create_function(|_, _: MultiValue| Ok(()))?)?;
    let globals = lua.globals();
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>| -> mlua::Result<()> {
        globals.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("ColorToHSV", |a| {
        let [r, g, b, _] = crate::surface::color_table(&a.table(1)?)?.map(|c| f64::from(c) / 255.0);
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == r {
            60.0 * ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        let saturation = if max == 0.0 { 0.0 } else { delta / max };
        (hue, saturation, max).into_lua_multi(a.lua)
    })?;
    def("HSVToColor", |a| {
        let (hue, saturation, value) = (a.number(1)?.rem_euclid(360.0), a.number(2)?.clamp(0.0, 1.0), a.number(3)?.clamp(0.0, 1.0));
        let chroma = value * saturation;
        let x = chroma * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match (hue / 60.0) as u32 {
            0 => (chroma, x, 0.0),
            1 => (x, chroma, 0.0),
            2 => (0.0, chroma, x),
            3 => (0.0, x, chroma),
            4 => (x, 0.0, chroma),
            _ => (chroma, 0.0, x),
        };
        let m = value - chroma;
        let channel = |c: f64| ((c + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        crate::surface::push_color(a.lua, [channel(r), channel(g), channel(b), 255]).map(|v| MultiValue::from_iter([v]))
    })?;
    Ok(())
}

pub(crate) fn add_network_string(lua: &Lua, name: &str) -> usize {
    let host = host(lua);
    let mut strings = host.network_strings.0.borrow_mut();
    if let Some(i) = strings.iter().position(|s| s.eq_ignore_ascii_case(name)) {
        return i + 1;
    }
    strings.push(name.to_string());
    strings.len()
}

fn bytes(a: &Args, position: usize) -> mlua::Result<Vec<u8>> {
    match a.get(position) {
        Some(Value::String(s)) => Ok(s.as_bytes().to_vec()),
        _ => Ok(a.string(position)?.into_bytes()),
    }
}

/// A KeyValues value as GMod hands it to Lua: numbers become numbers.
fn kv_scalar(lua: &Lua, text: &str) -> mlua::Result<Value> {
    if let Some(n) = lua.coerce_number(Value::String(lua.create_string(text)?))?
        && !text.trim().is_empty()
    {
        return Ok(Value::Number(n));
    }
    Ok(Value::String(lua.create_string(text)?))
}

pub(crate) fn kv_block(lua: &Lua, block: &gmod_kv::Block, ordered: bool, keep_case: bool) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for pair in block {
        let key = if keep_case { pair.key.clone() } else { pair.key.to_lowercase() };
        let value = match &pair.value {
            gmod_kv::Value::Str(text) => kv_scalar(lua, text)?,
            gmod_kv::Value::Block(inner) => Value::Table(kv_block(lua, inner, ordered, keep_case)?),
        };
        if ordered {
            let entry = lua.create_table()?;
            entry.raw_set("Key", key)?;
            entry.raw_set("Value", value)?;
            table.raw_push(entry)?;
        } else {
            let key = match lua.coerce_number(Value::String(lua.create_string(&key)?))? {
                Some(n) if n == n.trunc() => Value::Integer(n as i64),
                _ => Value::String(lua.create_string(&key)?),
            };
            table.raw_set(key, value)?;
        }
    }
    Ok(table)
}

/// `util.KeyValuesToTable(text [, escapes [, keepCase]])`: the block of the first key.
fn keyvalues_to_table(a: &Args, ordered: bool) -> mlua::Result<Value> {
    let text = a.string(1)?;
    let options = gmod_kv::Options { escapes: a.boolean(2) };
    let keep_case = a.boolean(3);
    let (document, _) = gmod_kv::parse_lenient(&text, options);
    let root = document.root.iter().find_map(|pair| pair.value.as_block());
    match root {
        Some(block) => Ok(Value::Table(kv_block(a.lua, block, ordered, keep_case)?)),
        None => Ok(Value::Table(a.lua.create_table()?)),
    }
}

fn quote_kv(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "'"))
}

fn scalar_text(lua: &Lua, value: &Value) -> mlua::Result<Option<String>> {
    Ok(match value {
        Value::String(s) => Some(s.to_string_lossy()),
        Value::Integer(_) | Value::Number(_) => lua.coerce_string(value.clone())?.map(|s| s.to_string_lossy()),
        Value::Boolean(b) => Some(if *b { "1" } else { "0" }.to_string()),
        Value::UserData(ud) => {
            if let Ok(v) = ud.borrow::<Vector>() {
                Some(format!("{} {} {}", v.x, v.y, v.z))
            } else if let Ok(a) = ud.borrow::<Angle>() {
                Some(format!("{} {} {}", a.p, a.y, a.r))
            } else {
                None
            }
        }
        _ => None,
    })
}

pub(crate) fn write_keyvalues(lua: &Lua, out: &mut String, name: &str, table: &Table, depth: usize) -> mlua::Result<()> {
    let indent = "\t".repeat(depth);
    let _ = writeln!(out, "{indent}{}", quote_kv(name));
    let _ = writeln!(out, "{indent}{{");
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Some(key) = scalar_text(lua, &key)? else { continue };
        match &value {
            Value::Table(inner) => write_keyvalues(lua, out, &key, inner, depth + 1)?,
            other => {
                if let Some(text) = scalar_text(lua, other)? {
                    let _ = writeln!(out, "{indent}\t{}\t\t{}", quote_kv(&key), quote_kv(&text));
                }
            }
        }
    }
    let _ = writeln!(out, "{indent}}}");
    Ok(())
}

fn json_string(out: &mut String, text: &[u8]) {
    out.push('"');
    for ch in String::from_utf8_lossy(text).chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_number(out: &mut String, n: f64) {
    if !n.is_finite() {
        out.push_str("null");
    } else if n == n.trunc() && n.abs() < 1e15 {
        let _ = write!(out, "{}", n as i64);
    } else {
        let _ = write!(out, "{n}");
    }
}

/// The keys 1..n, if a table is a sequence of n > 0 values and nothing else.
fn sequence_length(table: &Table) -> mlua::Result<Option<usize>> {
    let mut count = 0usize;
    for pair in table.pairs::<Value, Value>() {
        let (key, _) = pair?;
        match key {
            Value::Integer(i) if i >= 1 => count += 1,
            Value::Number(n) if n >= 1.0 && n == n.trunc() => count += 1,
            _ => return Ok(None),
        }
    }
    Ok((count > 0 && table.raw_len() == count).then_some(count))
}

fn write_json(lua: &Lua, out: &mut String, value: &Value, pretty: bool, depth: usize) -> mlua::Result<()> {
    let newline = |out: &mut String, depth: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&"\t".repeat(depth));
        }
    };
    match value {
        Value::Nil => out.push_str("null"),
        Value::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Integer(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Number(n) => json_number(out, *n),
        Value::String(s) => json_string(out, &s.as_bytes()),
        Value::UserData(ud) => {
            if let Ok(v) = ud.borrow::<Vector>() {
                json_string(out, format!("[{} {} {}]", v.x, v.y, v.z).as_bytes());
            } else if let Ok(a) = ud.borrow::<Angle>() {
                json_string(out, format!("{{{} {} {}}}", a.p, a.y, a.r).as_bytes());
            } else {
                out.push_str("null");
            }
        }
        Value::Table(table) => {
            if let Some(length) = sequence_length(table)? {
                out.push('[');
                for i in 1..=length {
                    if i > 1 {
                        out.push(',');
                    }
                    newline(out, depth + 1);
                    write_json(lua, out, &table.raw_get::<Value>(i)?, pretty, depth + 1)?;
                }
                newline(out, depth);
                out.push(']');
            } else {
                out.push('{');
                let mut first = true;
                for pair in table.pairs::<Value, Value>() {
                    let (key, value) = pair?;
                    if matches!(value, Value::Function(_) | Value::Thread(_)) {
                        continue;
                    }
                    let Some(key) = scalar_text(lua, &key)? else { continue };
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    newline(out, depth + 1);
                    json_string(out, key.as_bytes());
                    out.push(':');
                    if pretty {
                        out.push(' ');
                    }
                    write_json(lua, out, &value, pretty, depth + 1)?;
                }
                if !first {
                    newline(out, depth);
                }
                out.push('}');
            }
        }
        _ => out.push_str("null"),
    }
    Ok(())
}

struct Json<'a> {
    text: &'a [u8],
    at: usize,
    lua: &'a Lua,
    limits: bool,
    conversions: bool,
}

impl Json<'_> {
    fn document(&mut self) -> Option<Value> {
        let value = self.value(0)?;
        self.blank();
        (self.at == self.text.len()).then_some(value)
    }

    fn blank(&mut self) {
        while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.blank();
        if self.text.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if self.limits && depth > JSON_DEPTH {
            return None;
        }
        self.blank();
        match *self.text.get(self.at)? {
            b'{' => {
                self.at += 1;
                let table = self.lua.create_table().ok()?;
                if self.eat(b'}') {
                    return Some(Value::Table(table));
                }
                loop {
                    self.blank();
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return None;
                    }
                    let value = self.value(depth + 1)?;
                    let key = match key.parse::<f64>() {
                        Ok(n) if n == n.trunc() && n.abs() < 1e15 => Value::Integer(n as i64),
                        Ok(n) => Value::Number(n),
                        Err(_) => Value::String(self.lua.create_string(&key).ok()?),
                    };
                    table.raw_set(key, value).ok()?;
                    if self.eat(b'}') {
                        return Some(Value::Table(table));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let table = self.lua.create_table().ok()?;
                if self.eat(b']') {
                    return Some(Value::Table(table));
                }
                let mut index = 1i64;
                loop {
                    let value = self.value(depth + 1)?;
                    table.raw_set(index, value).ok()?;
                    index += 1;
                    if self.eat(b']') {
                        return Some(Value::Table(table));
                    }
                    if !self.eat(b',') {
                        return None;
                    }
                }
            }
            b'"' => {
                let text = self.string()?;
                if self.conversions
                    && let Some(converted) = self.convert(&text)
                {
                    return Some(converted);
                }
                Some(Value::String(self.lua.create_string(text).ok()?))
            }
            b't' if self.text[self.at..].starts_with(b"true") => {
                self.at += 4;
                Some(Value::Boolean(true))
            }
            b'f' if self.text[self.at..].starts_with(b"false") => {
                self.at += 5;
                Some(Value::Boolean(false))
            }
            b'n' if self.text[self.at..].starts_with(b"null") => {
                self.at += 4;
                Some(Value::Nil)
            }
            _ => {
                let start = self.at;
                while self.at < self.text.len() && matches!(self.text[self.at], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.at += 1;
                }
                let number: f64 = std::str::from_utf8(&self.text[start..self.at]).ok()?.parse().ok()?;
                Some(Value::Number(number))
            }
        }
    }

    /// `"[1 2 3]"` is a Vector, `"{1 2 3}"` an Angle.
    fn convert(&self, text: &str) -> Option<Value> {
        let (open, close) = (text.chars().next()?, text.chars().last()?);
        let inner = text.get(1..text.len() - 1)?;
        let numbers: Vec<f32> = inner.split_whitespace().map(|n| n.parse().ok()).collect::<Option<_>>()?;
        let [x, y, z] = numbers.try_into().ok()?;
        match (open, close) {
            ('[', ']') => self.lua.create_userdata(Vector::new(x, y, z)).ok().map(Value::UserData),
            ('{', '}') => self.lua.create_userdata(Angle::new(x, y, z)).ok().map(Value::UserData),
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.text.get(self.at) != Some(&b'"') {
            return None;
        }
        self.at += 1;
        let mut out = Vec::new();
        loop {
            let byte = *self.text.get(self.at)?;
            self.at += 1;
            match byte {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let escaped = *self.text.get(self.at)?;
                    self.at += 1;
                    match escaped {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let hex = std::str::from_utf8(self.text.get(self.at..self.at + 4)?).ok()?;
                            self.at += 4;
                            let mut code = u32::from_str_radix(hex, 16).ok()?;
                            // a surrogate pair
                            if (0xD800..0xDC00).contains(&code) && self.text.get(self.at..self.at + 2) == Some(b"\\u") {
                                let low = std::str::from_utf8(self.text.get(self.at + 2..self.at + 6)?).ok()?;
                                let low = u32::from_str_radix(low, 16).ok()?;
                                self.at += 6;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(code).unwrap_or('\u{FFFD}');
                            out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
    }
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64; GMod breaks the lines every 76 characters unless asked not to.
pub fn base64_encode(data: &[u8], line_breaks: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    if line_breaks && out.len() > 76 {
        let mut broken = String::with_capacity(out.len() + out.len() / 76 + 1);
        for (i, ch) in out.chars().enumerate() {
            if i > 0 && i % 76 == 0 {
                broken.push('\n');
            }
            broken.push(ch);
        }
        broken.push('\n');
        return broken;
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b if b.is_ascii_whitespace() => continue,
            _ => return None,
        };
        buffer = buffer << 6 | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

const STEAMID64_BASE: u64 = 76561197960265728;

fn steamid_to_64(id: &str) -> Option<u64> {
    let rest = id.trim().strip_prefix("STEAM_")?;
    let mut parts = rest.split(':');
    let _universe = parts.next()?;
    let low: u64 = parts.next()?.parse().ok()?;
    let account: u64 = parts.next()?.parse().ok()?;
    Some(STEAMID64_BASE + account * 2 + low)
}

fn steamid_from_64(id: u64) -> Option<String> {
    let account = id.checked_sub(STEAMID64_BASE)?;
    Some(format!("STEAM_0:{}:{}", account % 2, account / 2))
}

/// `util.SpriteTrail(ent, attachment, color, additive, start_width, end_width, lifetime,
/// texture_res, texture)`: an `env_spritetrail` that follows the entity, made and given back.
fn sprite_trail(a: &Args) -> mlua::Result<Value> {
    let target = crate::entity::this(a, 1)?;
    let number = |position: usize| a.opt_number(position).ok().flatten().unwrap_or(0.0);
    let color = match a.get(3) {
        Some(Value::Table(c)) => {
            let part = |key: &str| c.get::<Option<f64>>(key).ok().flatten().unwrap_or(255.0);
            [part("r"), part("g"), part("b"), part("a")]
        }
        _ => [255.0; 4],
    };
    let texture = a.opt_string(9)?.unwrap_or_default();
    let Some(trail) = crate::ents::create(a.lua, "env_spritetrail", None)? else { return Ok(Value::Nil) };
    let pos = crate::host(a.lua).entities.transform(target).map(|(pos, _)| pos).unwrap_or_default();
    crate::entity::set_world_pos(a.lua, trail, pos);
    let render_mode = if a.boolean(4) { "5" } else { "1" };
    for (key, value) in [
        ("lifetime", number(7).to_string()),
        ("startwidth", number(5).to_string()),
        ("endwidth", number(6).to_string()),
        ("spritename", texture),
        ("rendermode", render_mode.to_string()),
        ("rendercolor", format!("{} {} {}", color[0], color[1], color[2])),
        ("renderamt", color[3].to_string()),
    ] {
        crate::ents::key_value(a.lua, trail, key, &value);
    }
    crate::entity::set_parent(a.lua, trail, target);
    crate::ents::spawn(a.lua, trail);
    crate::entity::push(a.lua, trail)
}
