//! Smaller parts of the API the engine has: blast damage, a gun's flash, `ServerLog`, hashes,
//! the shared dice of prediction, aiming through the screen, the geometry `util` tests, and
//! the odd global.

use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::damage::{DamageInfo, dmg};
use crate::effect::EffectData;
use crate::entity::{self, EntityHandle};
use crate::vector::{Angle, Vector};
use crate::{Realm, host};

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn def_multi(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn vector(a: &Args, position: usize) -> mlua::Result<Vector> {
    a.userdata::<Vector>(position, "Vector")
}

fn angle(a: &Args, position: usize) -> mlua::Result<Angle> {
    a.userdata::<Angle>(position, "Angle")
}

fn float(a: &Args, position: usize) -> mlua::Result<f32> {
    Ok(a.number(position)? as f32)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// MD5, which nothing else here has.
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x6745_2301u32, 0xefcd_ab89u32, 0x98ba_dcfeu32, 0x1032_5476u32);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for chunk in message.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, word) in [a0, b0, c0, d0].into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn bytes(a: &Args, position: usize) -> mlua::Result<Vec<u8>> {
    match a.get(position) {
        Some(Value::String(s)) => Ok(s.as_bytes().to_vec()),
        _ => Ok(a.string(position)?.into_bytes()),
    }
}

/// Where a ray `start + t * delta` (`t` from 0 to 1) meets a box turned by `angles` about
/// `origin`: the point, the face's normal, and `t`.
fn ray_obb(start: Vector, delta: Vector, origin: Vector, angles: Angle, mins: Vector, maxs: Vector) -> Option<(Vector, Vector, f32)> {
    let (forward, right, up) = angles.vectors();
    let axes = [forward, -right, up];
    let local = |v: Vector| Vector::new(v.dot(axes[0]), v.dot(axes[1]), v.dot(axes[2]));
    let (s, d) = (local(start - origin), local(delta));
    let (lo, hi) = ([mins.x, mins.y, mins.z], [maxs.x, maxs.y, maxs.z]);
    let (sv, dv) = ([s.x, s.y, s.z], [d.x, d.y, d.z]);
    let (mut enter, mut leave, mut axis, mut sign) = (f32::NEG_INFINITY, f32::INFINITY, 0, -1.0);
    for i in 0..3 {
        if dv[i].abs() < 1e-8 {
            if sv[i] < lo[i] || sv[i] > hi[i] {
                return None;
            }
            continue;
        }
        let (mut t0, mut t1) = ((lo[i] - sv[i]) / dv[i], (hi[i] - sv[i]) / dv[i]);
        let mut face = -1.0;
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
            face = 1.0;
        }
        if t0 > enter {
            enter = t0;
            axis = i;
            sign = face;
        }
        leave = leave.min(t1);
    }
    let t = enter.max(0.0);
    if enter > leave || t > 1.0 || leave < 0.0 {
        return None;
    }
    let normal = axes[axis] * sign;
    Some((start + delta * t, normal, t))
}

/// The corners of a box turned by `angles` about `origin`.
fn corners(origin: Vector, angles: Angle, mins: Vector, maxs: Vector) -> (Vector, [Vector; 3], Vector) {
    let (forward, right, up) = angles.vectors();
    let axes = [forward, -right, up];
    let center = (mins + maxs) * 0.5;
    let half = (maxs - mins) * 0.5;
    (origin + axes[0] * center.x + axes[1] * center.y + axes[2] * center.z, axes, half)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let util: Table = globals.get("util")?;
    let realm = host(lua).realm;
    if realm == Realm::Server {
        // `util.BlastDamage(inflictor, attacker, origin, radius, damage)`
        def(lua, &util, "BlastDamage", |a| {
            let inflictor = entity::handle_of(a.get(1)).unwrap_or(EntityHandle::NULL);
            let attacker = entity::handle_of(a.get(2)).unwrap_or(EntityHandle::NULL);
            let (origin, radius, damage) = (vector(a, 3)?, float(a, 4)?, float(a, 5)?);
            crate::breakable::radius_damage(a.lua, DamageInfo::new(damage, dmg::BLAST, attacker, inflictor), origin, radius);
            Ok(Value::Nil)
        })?;
        def(lua, &util, "BlastDamageInfo", |a| {
            let info = a.userdata::<DamageInfo>(1, "CTakeDamageInfo")?;
            let (origin, radius) = (vector(a, 2)?, float(a, 3)?);
            crate::breakable::radius_damage(a.lua, info, origin, radius);
            Ok(Value::Nil)
        })?;
        // the server's log is its console
        def(lua, &globals, "ServerLog", |a| {
            let text = a.string(1)?;
            host(a.lua).print(&text);
            Ok(Value::Nil)
        })?;
    }
    def(lua, &util, "MD5", |a| Ok(Value::String(a.lua.create_string(hex(&md5(&bytes(a, 1)?)))?)))?;
    def(lua, &util, "SHA1", |a| {
        let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &bytes(a, 1)?);
        Ok(Value::String(a.lua.create_string(hex(digest.as_ref()))?))
    })?;
    def(lua, &util, "SHA256", |a| {
        let digest = ring::digest::digest(&ring::digest::SHA256, &bytes(a, 1)?);
        Ok(Value::String(a.lua.create_string(hex(digest.as_ref()))?))
    })?;
    // the same on the server and the client predicting: of the name, the tick and the seed given
    def(lua, &util, "SharedRandom", |a| {
        let name = a.string(1)?;
        let (low, high) = (a.number(2)?, a.number(3)?);
        let seed = a.opt_number(4)?.unwrap_or(0.0) as i64;
        let tick = (host(a.lua).clock.cur_time.get() / crate::timer::TICK_INTERVAL).round() as i64;
        let mut x = u64::from(gmod_fs::crc32::crc32(format!("{name}{tick}{seed}").as_bytes())) | 1;
        for _ in 0..3 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
        }
        let fraction = (x >> 11) as f64 / (1u64 << 53) as f64;
        Ok(Value::Number(low + (high - low) * fraction))
    })?;
    // the way through a point of the screen: angles, field of view, x, y, width, height
    def(lua, &util, "AimVector", |a| {
        let (angles, fov) = (angle(a, 1)?, float(a, 2)?);
        let (x, y, w, h) = (float(a, 3)?, float(a, 4)?, float(a, 5)?.max(1.0), float(a, 6)?.max(1.0));
        let (forward, right, up) = angles.vectors();
        let tangent = (fov.to_radians() / 2.0).tan();
        let dx = (2.0 * x / w - 1.0) * tangent;
        let dy = (1.0 - 2.0 * y / h) * tangent * (h / w);
        entity::vector(a.lua, (forward + right * dx + up * dy).normalized())
    })?;
    def_multi(lua, &util, "IntersectRayWithOBB", |a| {
        let hit = ray_obb(vector(a, 1)?, vector(a, 2)?, vector(a, 3)?, angle(a, 4)?, vector(a, 5)?, vector(a, 6)?);
        match hit {
            Some((pos, normal, t)) => (entity::vector(a.lua, pos)?, entity::vector(a.lua, normal)?, f64::from(t)).into_lua_multi(a.lua),
            None => Ok(MultiValue::from_iter([Value::Nil])),
        }
    })?;
    def_multi(lua, &util, "IntersectRayWithSphere", |a| {
        let (origin, delta, center, radius) = (vector(a, 1)?, vector(a, 2)?, vector(a, 3)?, float(a, 4)?);
        let to = origin - center;
        let (qa, qb, qc) = (delta.dot(delta), 2.0 * to.dot(delta), to.dot(to) - radius * radius);
        let disc = qb * qb - 4.0 * qa * qc;
        if qa < 1e-8 || disc < 0.0 {
            return Ok(MultiValue::from_iter([Value::Nil]));
        }
        let root = disc.sqrt();
        let (t1, t2) = ((-qb - root) / (2.0 * qa), (-qb + root) / (2.0 * qa));
        if t2 < 0.0 || t1 > 1.0 {
            return Ok(MultiValue::from_iter([Value::Nil]));
        }
        (f64::from(t1), f64::from(t2)).into_lua_multi(a.lua)
    })?;
    def(lua, &util, "IntersectRayWithTriangle", |a| {
        let (origin, delta) = (vector(a, 1)?, vector(a, 2)?);
        let (p1, p2, p3) = (vector(a, 3)?, vector(a, 4)?, vector(a, 5)?);
        let (e1, e2) = (p2 - p1, p3 - p1);
        let h = delta.cross(e2);
        let det = e1.dot(h);
        if det.abs() < 1e-8 {
            return Ok(Value::Boolean(false));
        }
        let s = origin - p1;
        let u = s.dot(h) / det;
        let q = s.cross(e1);
        let v = delta.dot(q) / det;
        let t = e2.dot(q) / det;
        if !(0.0..=1.0).contains(&u) || v < 0.0 || u + v > 1.0 || !(0.0..=1.0).contains(&t) {
            return Ok(Value::Boolean(false));
        }
        entity::vector(a.lua, origin + delta * t)
    })?;
    def(lua, &util, "IsBoxIntersectingBox", |a| {
        let (lo1, hi1, lo2, hi2) = (vector(a, 1)?, vector(a, 2)?, vector(a, 3)?, vector(a, 4)?);
        Ok(Value::Boolean(lo1.x <= hi2.x && hi1.x >= lo2.x && lo1.y <= hi2.y && hi1.y >= lo2.y && lo1.z <= hi2.z && hi1.z >= lo2.z))
    })?;
    def(lua, &util, "IsBoxIntersectingSphere", |a| {
        let (lo, hi, center, radius) = (vector(a, 1)?, vector(a, 2)?, vector(a, 3)?, float(a, 4)?);
        let nearest = Vector::new(center.x.clamp(lo.x, hi.x), center.y.clamp(lo.y, hi.y), center.z.clamp(lo.z, hi.z));
        Ok(Value::Boolean((nearest - center).length() <= radius))
    })?;
    def(lua, &util, "IsSphereIntersectingSphere", |a| {
        let (c1, r1, c2, r2) = (vector(a, 1)?, float(a, 2)?, vector(a, 3)?, float(a, 4)?);
        Ok(Value::Boolean((c1 - c2).length() <= r1 + r2))
    })?;
    // `IsPointInCone(point, origin, axis, sine, length)`
    def(lua, &util, "IsPointInCone", |a| {
        let (point, origin, axis, sine, length) = (vector(a, 1)?, vector(a, 2)?, vector(a, 3)?.normalized(), float(a, 4)?, float(a, 5)?);
        let to = point - origin;
        let along = to.dot(axis);
        if along < 0.0 || along > length {
            return Ok(Value::Boolean(false));
        }
        let distance = to.length();
        let cosine = (1.0 - sine * sine).max(0.0).sqrt();
        Ok(Value::Boolean(distance < 1e-6 || along / distance >= cosine))
    })?;
    // `IsSphereIntersectingCone(center, radius, origin, axis, sine, cosine)`
    def(lua, &util, "IsSphereIntersectingCone", |a| {
        let (center, radius, origin, axis) = (vector(a, 1)?, float(a, 2)?, vector(a, 3)?, vector(a, 4)?.normalized());
        let (sine, cosine) = (float(a, 5)?, float(a, 6)?);
        let to = center - origin;
        let along = to.dot(axis);
        let across = (to - axis * along).length();
        // the distance from the sphere's middle to the cone's side
        Ok(Value::Boolean(across * cosine - along * sine <= radius && along >= -radius))
    })?;
    // `IsOBBIntersectingOBB(pos1, ang1, mins1, maxs1, pos2, ang2, mins2, maxs2, tolerance)`: the
    // separating axis test
    def(lua, &util, "IsOBBIntersectingOBB", |a| {
        let (c1, ax1, h1) = corners(vector(a, 1)?, angle(a, 2)?, vector(a, 3)?, vector(a, 4)?);
        let (c2, ax2, h2) = corners(vector(a, 5)?, angle(a, 6)?, vector(a, 7)?, vector(a, 8)?);
        let tolerance = a.opt_number(9)?.unwrap_or(0.0) as f32;
        let (e1, e2) = ([h1.x, h1.y, h1.z], [h2.x, h2.y, h2.z]);
        let d = c2 - c1;
        let mut axes: Vec<Vector> = ax1.iter().chain(ax2.iter()).copied().collect();
        for a1 in ax1 {
            for a2 in ax2 {
                let c = a1.cross(a2);
                if c.length() > 1e-5 {
                    axes.push(c.normalized());
                }
            }
        }
        let apart = axes.iter().any(|&l| {
            let r1: f32 = (0..3).map(|i| e1[i] * ax1[i].dot(l).abs()).sum();
            let r2: f32 = (0..3).map(|i| e2[i] * ax2[i].dot(l).abs()).sum();
            d.dot(l).abs() > r1 + r2 + tolerance
        });
        Ok(Value::Boolean(!apart))
    })?;
    // milliseconds since the last call
    def(lua, &util, "TimerCycle", |a| {
        let now = host(a.lua).started.elapsed().as_secs_f64();
        let last = host(a.lua).game.timer_cycle.replace(now);
        Ok(Value::Number((now - last) * 1000.0))
    })?;

    // a gun's flash: the client's own as it predicts, the server's for the others
    let ent = crate::meta::meta_table(lua, "Entity", crate::meta::type_id::ENTITY)?;
    def(lua, &ent, "MuzzleFlash", |a| {
        let handle = entity::handle_of(a.get(1)).filter(|&h| host(a.lua).entities.is_valid(h)).ok_or_else(|| a.bad(1, "Entity"))?;
        // a player's or an NPC's flash is at its gun
        let gun = crate::player::active_weapon(a.lua, handle).unwrap_or(handle);
        let data = EffectData {
            entity: gun,
            attachment: 1,
            origin: host(a.lua).entities.transform(gun).map_or(Vector::default(), |(p, _)| p),
            ..EffectData::default()
        };
        if host(a.lua).realm == Realm::Server {
            crate::effect::send(a.lua, "MuzzleFlash", data, None);
        } else if crate::timer::first_time_predicted(a.lua) {
            crate::effect::make(a.lua, "MuzzleFlash", data);
        }
        Ok(Value::Nil)
    })?;

    let vector_meta = crate::meta::meta_table(lua, "Vector", crate::meta::type_id::VECTOR)?;
    def(lua, &vector_meta, "Random", |a| {
        let (low, high) = (a.opt_number(2)?.unwrap_or(-1.0) as f32, a.opt_number(3)?.unwrap_or(1.0) as f32);
        let v = Vector::new(
            crate::particles::rand(a.lua, low, high),
            crate::particles::rand(a.lua, low, high),
            crate::particles::rand(a.lua, low, high),
        );
        a.change::<Vector>(1, "Vector", |own| *own = v)
    })?;
    let angle_meta = crate::meta::meta_table(lua, "Angle", crate::meta::type_id::ANGLE)?;
    def(lua, &angle_meta, "Random", |a| {
        let (low, high) = (a.opt_number(2)?.unwrap_or(-360.0) as f32, a.opt_number(3)?.unwrap_or(360.0) as f32);
        let v = Angle::new(
            crate::particles::rand(a.lua, low, high),
            crate::particles::rand(a.lua, low, high),
            crate::particles::rand(a.lua, low, high),
        );
        a.change::<Angle>(1, "Angle", |own| *own = v)
    })?;

    // the player whose command runs now: the server's, or the client's own as it predicts
    def(lua, &globals, "GetPredictionPlayer", |a| {
        let host = host(a.lua);
        let player = match host.realm {
            Realm::Server => host.players.commanding.get(),
            _ => host.clock.unpredicted.get().map(|_| host.client.local_player.get()),
        };
        drop(host);
        entity::push(a.lua, player.unwrap_or(EntityHandle::NULL))
    })?;
    def(lua, &globals, "IsConCommandBlocked", |_| Ok(Value::Boolean(false)))?;
    def(lua, &globals, "FrameNumber", |a| Ok(Value::Integer(host(a.lua).clock.ticks.get() as i64)))?;
    def(lua, &globals, "VGUIFrameTime", |a| Ok(Value::Number(host(a.lua).started.elapsed().as_secs_f64())))?;
    def(lua, &globals, "DropEntityIfHeld", |a| {
        if let Some(handle) = entity::handle_of(a.get(1)) {
            crate::pickup::release(a.lua, handle);
        }
        Ok(Value::Nil)
    })?;
    // `ColorToHSL(color)`: hue in degrees, saturation and lightness from 0 to 1
    def_multi(lua, &globals, "ColorToHSL", |a| {
        let t = a.table(1)?;
        let channel = |k: &str| t.get::<Option<f64>>(k).ok().flatten().unwrap_or(0.0) / 255.0;
        let (r, g, b) = (channel("r"), channel("g"), channel("b"));
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let l = (max + min) / 2.0;
        let d = max - min;
        let (h, s) = if d < 1e-9 {
            (0.0, 0.0)
        } else {
            let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
            let h = if max == r {
                (g - b) / d + if g < b { 6.0 } else { 0.0 }
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
            (h * 60.0, s)
        };
        (h, s, l).into_lua_multi(a.lua)
    })?;
    if realm != Realm::Server {
        def(lua, &globals, "SetClipboardText", |a| {
            let text = a.string(1)?;
            host(a.lua).panels.set_clipboard(text);
            Ok(Value::Nil)
        })?;
    }

    // the names old scripts use
    lua.load("math.mod = math.mod or math.fmod string.gfind = string.gfind or string.gmatch").set_name("=[openGMOD aliases]").exec()?;
    Ok(())
}
