//! `Vector` and `Angle`, GMod's 3D math.
//!
//! As in GMod (and Source), both hold three 32-bit floats: `Vector(0.1, 0, 0).x` is
//! `0.10000000149011612`, not `0.1`. Angles are pitch, yaw, roll in degrees; the axes are
//! Source's (X forward, Y left, Z up), and the conversions between angles and directions are
//! Source's `AngleVectors` and `VectorAngles`.

use mlua::{AnyUserData, IntoLuaMulti, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::{self, Args};
use crate::meta::{self, type_id};

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Vector {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Angle {
    pub p: f32,
    pub y: f32,
    pub r: f32,
}

impl Vector {
    pub const fn new(x: f32, y: f32, z: f32) -> Vector {
        Vector { x, y, z }
    }

    fn map(self, f: impl Fn(f32) -> f32) -> Vector {
        Vector::new(f(self.x), f(self.y), f(self.z))
    }

    fn zip(self, o: Vector, f: impl Fn(f32, f32) -> f32) -> Vector {
        Vector::new(f(self.x, o.x), f(self.y, o.y), f(self.z, o.z))
    }

    pub fn dot(self, o: Vector) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vector) -> Vector {
        Vector::new(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    /// The vector scaled to length 1; the zero vector stays zero.
    pub fn normalized(self) -> Vector {
        let length = self.length();
        if length > f32::EPSILON { self.map(|c| c / length) } else { Vector::default() }
    }

    /// Source's `VectorAngles`: the angle that looks along this direction, without roll.
    pub fn angle(self) -> Angle {
        if self.y == 0.0 && self.x == 0.0 {
            return Angle::new(if self.z > 0.0 { 270.0 } else { 90.0 }, 0.0, 0.0);
        }
        let mut yaw = self.y.atan2(self.x).to_degrees();
        if yaw < 0.0 {
            yaw += 360.0;
        }
        let horizontal = (self.x * self.x + self.y * self.y).sqrt();
        let mut pitch = (-self.z).atan2(horizontal).to_degrees();
        if pitch < 0.0 {
            pitch += 360.0;
        }
        Angle::new(pitch, yaw, 0.0)
    }

    /// Source's `VectorAngles` with an up direction: the roll too.
    pub fn angle_with_up(self, up: Vector) -> Angle {
        let left = up.cross(self).normalized();
        let horizontal = (self.x * self.x + self.y * self.y).sqrt();
        if horizontal > 0.001 {
            let yaw = self.y.atan2(self.x).to_degrees();
            let pitch = (-self.z).atan2(horizontal).to_degrees();
            let roll = left.z.atan2(left.y * self.x - left.x * self.y).to_degrees();
            Angle::new(pitch, yaw, roll)
        } else {
            let yaw = (-left.x).atan2(left.y).to_degrees();
            let pitch = (-self.z).atan2(horizontal).to_degrees();
            Angle::new(pitch, yaw, 0.0)
        }
    }

    /// The vector turned by an angle: from the angle's local space into the world.
    pub fn rotated(self, angle: Angle) -> Vector {
        let (forward, right, up) = angle.vectors();
        let left = -right;
        forward * self.x + left * self.y + up * self.z
    }

    /// Rodrigues' rotation about a unit axis.
    fn rotated_about(self, axis: Vector, degrees: f32) -> Vector {
        let (sin, cos) = degrees.to_radians().sin_cos();
        self * cos + axis.cross(self) * sin + axis * (axis.dot(self) * (1.0 - cos))
    }
}

impl std::ops::Add for Vector {
    type Output = Vector;
    fn add(self, o: Vector) -> Vector {
        self.zip(o, |a, b| a + b)
    }
}

impl std::ops::Sub for Vector {
    type Output = Vector;
    fn sub(self, o: Vector) -> Vector {
        self.zip(o, |a, b| a - b)
    }
}

impl std::ops::Mul<f32> for Vector {
    type Output = Vector;
    fn mul(self, s: f32) -> Vector {
        self.map(|c| c * s)
    }
}

impl std::ops::Neg for Vector {
    type Output = Vector;
    fn neg(self) -> Vector {
        self.map(|c| -c)
    }
}

/// An angle component brought into [-180, 180], as Source's `AngleNormalize`.
pub(crate) fn normalize_degrees(angle: f32) -> f32 {
    let mut angle = angle % 360.0;
    if angle > 180.0 {
        angle -= 360.0;
    }
    if angle < -180.0 {
        angle += 360.0;
    }
    angle
}

impl Angle {
    pub const fn new(p: f32, y: f32, r: f32) -> Angle {
        Angle { p, y, r }
    }

    fn map(self, f: impl Fn(f32) -> f32) -> Angle {
        Angle::new(f(self.p), f(self.y), f(self.r))
    }

    fn zip(self, o: Angle, f: impl Fn(f32, f32) -> f32) -> Angle {
        Angle::new(f(self.p, o.p), f(self.y, o.y), f(self.r, o.r))
    }

    /// Source's `AngleVectors`: forward, right and up.
    pub fn vectors(self) -> (Vector, Vector, Vector) {
        let (sp, cp) = self.p.to_radians().sin_cos();
        let (sy, cy) = self.y.to_radians().sin_cos();
        let (sr, cr) = self.r.to_radians().sin_cos();
        let forward = Vector::new(cp * cy, cp * sy, -sp);
        let right = Vector::new(-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp);
        let up = Vector::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp);
        (forward, right, up)
    }

    pub fn normalized(self) -> Angle {
        self.map(normalize_degrees)
    }

    /// The angle turned about an axis (in the world) by `degrees`.
    pub fn rotated_about(self, axis: Vector, degrees: f32) -> Angle {
        let axis = axis.normalized();
        let (forward, _, up) = self.vectors();
        forward.rotated_about(axis, degrees).angle_with_up(up.rotated_about(axis, degrees))
    }
}

fn vector_of(value: &Value) -> Option<Vector> {
    match value {
        Value::UserData(ud) => ud.borrow::<Vector>().ok().map(|v| *v),
        _ => None,
    }
}

fn angle_of(value: &Value) -> Option<Angle> {
    match value {
        Value::UserData(ud) => ud.borrow::<Angle>().ok().map(|a| *a),
        _ => None,
    }
}

fn number_of(value: &Value) -> Option<f32> {
    match value {
        Value::Integer(n) => Some(*n as f32),
        Value::Number(n) => Some(*n as f32),
        _ => None,
    }
}

/// `1`, `2`, `3` or one of three names: which component a key means.
fn component(key: &Value, names: [&[&str]; 3]) -> Option<usize> {
    match key {
        Value::Integer(i @ 1..=3) => Some(*i as usize - 1),
        Value::Number(n) if [1.0, 2.0, 3.0].contains(n) => Some(*n as usize - 1),
        Value::String(s) => {
            let s = s.as_bytes();
            names.iter().position(|options| options.iter().any(|name| name.as_bytes() == &s[..]))
        }
        _ => None,
    }
}

const VECTOR_KEYS: [&[&str]; 3] = [&["x"], &["y"], &["z"]];
const ANGLE_KEYS: [&[&str]; 3] = [&["p", "pitch"], &["y", "yaw"], &["r", "roll"]];

fn operand_error(lua: &Lua, op: &str, a: &Value, b: &Value) -> mlua::Error {
    args::error(lua, format!("attempt to perform arithmetic ({op}) on {} and {}", args::type_name(Some(a)), args::type_name(Some(b))))
}

impl UserData for Vector {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "Vector");
        fields.add_meta_field("MetaID", type_id::VECTOR);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| {
            if let Some(i) = component(&key, VECTOR_KEYS) {
                let v = *ud.borrow::<Vector>()?;
                return Ok(Value::Number([v.x, v.y, v.z][i] as f64));
            }
            meta::lookup(lua, "Vector", key)
        });
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            let Some(i) = component(&key, VECTOR_KEYS) else { return Ok(()) };
            let n = lua.coerce_number(value)?.ok_or_else(|| args::error(lua, "Vector components are numbers"))? as f32;
            let mut v = ud.borrow_mut::<Vector>()?;
            let v = &mut *v;
            *[&mut v.x, &mut v.y, &mut v.z][i] = n;
            Ok(())
        });
        methods.add_meta_function("__add", |lua, (a, b): (Value, Value)| match (vector_of(&a), vector_of(&b)) {
            (Some(a), Some(b)) => Ok(a + b),
            _ => Err(operand_error(lua, "add", &a, &b)),
        });
        methods.add_meta_function("__sub", |lua, (a, b): (Value, Value)| match (vector_of(&a), vector_of(&b)) {
            (Some(a), Some(b)) => Ok(a - b),
            _ => Err(operand_error(lua, "sub", &a, &b)),
        });
        methods.add_meta_function("__mul", |lua, (a, b): (Value, Value)| {
            match (vector_of(&a), vector_of(&b), number_of(&a), number_of(&b)) {
                (Some(v), Some(w), ..) => Ok(v.zip(w, |a, b| a * b)),
                (Some(v), None, _, Some(s)) | (None, Some(v), Some(s), _) => Ok(v * s),
                _ => Err(operand_error(lua, "mul", &a, &b)),
            }
        });
        methods.add_meta_function("__div", |lua, (a, b): (Value, Value)| match (vector_of(&a), vector_of(&b), number_of(&b)) {
            (Some(v), Some(w), _) => Ok(v.zip(w, |a, b| a / b)),
            (Some(v), None, Some(s)) => Ok(v.map(|c| c / s)),
            _ => Err(operand_error(lua, "div", &a, &b)),
        });
        methods.add_meta_function("__unm", |_, v: AnyUserData| Ok(-*v.borrow::<Vector>()?));
        methods.add_meta_function("__eq", |_, (a, b): (Value, Value)| Ok(vector_of(&a).is_some() && vector_of(&a) == vector_of(&b)));
        methods.add_meta_function("__tostring", |_, v: AnyUserData| {
            let v = *v.borrow::<Vector>()?;
            Ok(format!("{:.6} {:.6} {:.6}", v.x, v.y, v.z))
        });
    }
}

impl UserData for Angle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "Angle");
        fields.add_meta_field("MetaID", type_id::ANGLE);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| {
            if let Some(i) = component(&key, ANGLE_KEYS) {
                let a = *ud.borrow::<Angle>()?;
                return Ok(Value::Number([a.p, a.y, a.r][i] as f64));
            }
            meta::lookup(lua, "Angle", key)
        });
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| {
            let Some(i) = component(&key, ANGLE_KEYS) else { return Ok(()) };
            let n = lua.coerce_number(value)?.ok_or_else(|| args::error(lua, "Angle components are numbers"))? as f32;
            let mut a = ud.borrow_mut::<Angle>()?;
            let a = &mut *a;
            *[&mut a.p, &mut a.y, &mut a.r][i] = n;
            Ok(())
        });
        methods.add_meta_function("__add", |lua, (a, b): (Value, Value)| match (angle_of(&a), angle_of(&b)) {
            (Some(a), Some(b)) => Ok(a.zip(b, |x, y| x + y)),
            _ => Err(operand_error(lua, "add", &a, &b)),
        });
        methods.add_meta_function("__sub", |lua, (a, b): (Value, Value)| match (angle_of(&a), angle_of(&b)) {
            (Some(a), Some(b)) => Ok(a.zip(b, |x, y| x - y)),
            _ => Err(operand_error(lua, "sub", &a, &b)),
        });
        methods.add_meta_function("__mul", |lua, (a, b): (Value, Value)| {
            match (angle_of(&a), angle_of(&b), number_of(&a), number_of(&b)) {
                (Some(x), None, _, Some(s)) | (None, Some(x), Some(s), _) => Ok(x.map(|c| c * s)),
                _ => Err(operand_error(lua, "mul", &a, &b)),
            }
        });
        methods.add_meta_function("__div", |lua, (a, b): (Value, Value)| match (angle_of(&a), number_of(&b)) {
            (Some(x), Some(s)) => Ok(x.map(|c| c / s)),
            _ => Err(operand_error(lua, "div", &a, &b)),
        });
        methods.add_meta_function("__unm", |_, a: AnyUserData| Ok(a.borrow::<Angle>()?.map(|c| -c)));
        methods.add_meta_function("__eq", |_, (a, b): (Value, Value)| Ok(angle_of(&a).is_some() && angle_of(&a) == angle_of(&b)));
        methods.add_meta_function("__tostring", |_, a: AnyUserData| {
            let a = *a.borrow::<Angle>()?;
            Ok(format!("{:.3} {:.3} {:.3}", a.p, a.y, a.r))
        });
    }
}

/// Puts a method into a named metatable.
fn def<R: IntoLuaMulti>(lua: &Lua, table: &Table, name: &'static str, f: impl Fn(&Args) -> mlua::Result<R> + 'static) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

/// The vector a method was called on, to change.
fn this_vector(args: &Args) -> mlua::Result<mlua::UserDataRefMut<Vector>> {
    match args.get(1) {
        Some(Value::UserData(ud)) => ud.borrow_mut::<Vector>().map_err(|_| args.bad(1, "Vector")),
        _ => Err(args.bad(1, "Vector")),
    }
}

fn this_angle(args: &Args) -> mlua::Result<mlua::UserDataRefMut<Angle>> {
    match args.get(1) {
        Some(Value::UserData(ud)) => ud.borrow_mut::<Angle>().map_err(|_| args.bad(1, "Angle")),
        _ => Err(args.bad(1, "Angle")),
    }
}

fn f32_arg(args: &Args, position: usize) -> mlua::Result<f32> {
    args.number(position).map(|n| n as f32)
}

/// Three numbers from `"1 2 3"`, as GMod's constructors read a string; what does not parse is 0.
fn parse_three(text: &str) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (slot, part) in out.iter_mut().zip(text.split_whitespace()) {
        *slot = part.parse().unwrap_or(0.0);
    }
    out
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    globals.raw_set(
        "Vector",
        lua.create_function(|lua, values: MultiValue| {
            let args = Args::new(lua, values, "Vector");
            match args.get(1) {
                Some(Value::UserData(_)) => args.userdata::<Vector>(1, "Vector"),
                Some(Value::String(s)) if lua.coerce_number(Value::String(s.clone()))?.is_none() => {
                    let [x, y, z] = parse_three(&s.to_string_lossy());
                    Ok(Vector::new(x, y, z))
                }
                _ => Ok(Vector::new(
                    args.opt_number(1)?.unwrap_or(0.0) as f32,
                    args.opt_number(2)?.unwrap_or(0.0) as f32,
                    args.opt_number(3)?.unwrap_or(0.0) as f32,
                )),
            }
        })?,
    )?;
    globals.raw_set(
        "Angle",
        lua.create_function(|lua, values: MultiValue| {
            let args = Args::new(lua, values, "Angle");
            match args.get(1) {
                Some(Value::UserData(_)) => args.userdata::<Angle>(1, "Angle"),
                Some(Value::String(s)) if lua.coerce_number(Value::String(s.clone()))?.is_none() => {
                    let [p, y, r] = parse_three(&s.to_string_lossy());
                    Ok(Angle::new(p, y, r))
                }
                _ => Ok(Angle::new(
                    args.opt_number(1)?.unwrap_or(0.0) as f32,
                    args.opt_number(2)?.unwrap_or(0.0) as f32,
                    args.opt_number(3)?.unwrap_or(0.0) as f32,
                )),
            }
        })?,
    )?;
    // a position and angles of one space in another: two values each
    def(lua, &globals, "LocalToWorld", |a| {
        let (pos, angles) = (a.userdata::<Vector>(1, "Vector")?, a.userdata::<Angle>(2, "Angle")?);
        let (origin, turned) = (a.userdata::<Vector>(3, "Vector")?, a.userdata::<Angle>(4, "Angle")?);
        Ok((origin + pos.rotated(turned), crate::entity::compose(turned, angles)))
    })?;
    def(lua, &globals, "WorldToLocal", |a| {
        let (pos, angles) = (a.userdata::<Vector>(1, "Vector")?, a.userdata::<Angle>(2, "Angle")?);
        let (origin, turned) = (a.userdata::<Vector>(3, "Vector")?, a.userdata::<Angle>(4, "Angle")?);
        Ok((crate::entity::unrotated(pos - origin, turned), crate::entity::decompose(turned, angles)))
    })?;
    // the first takes the smaller of each, the second the larger
    def(lua, &globals, "OrderVectors", |a| {
        let (first, second) = (a.userdata::<Vector>(1, "Vector")?, a.userdata::<Vector>(2, "Vector")?);
        let lo = Vector::new(first.x.min(second.x), first.y.min(second.y), first.z.min(second.z));
        let hi = Vector::new(first.x.max(second.x), first.y.max(second.y), first.z.max(second.z));
        for (position, value) in [(1, lo), (2, hi)] {
            if let Some(Value::UserData(ud)) = a.get(position) {
                *ud.borrow_mut::<Vector>()? = value;
            }
        }
        Ok(())
    })?;
    // the engine's linear interpolations, which Derma's animations (MoveTo, SizeTo) go through
    def(lua, &globals, "LerpVector", |a| {
        let t = a.number(1)? as f32;
        let (from, to) = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        Ok(Vector::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t, from.z + (to.z - from.z) * t))
    })?;
    // each angle the short way round
    def(lua, &globals, "LerpAngle", |a| {
        let t = a.number(1)? as f32;
        let (from, to) = (a.userdata::<Angle>(2, "Angle")?, a.userdata::<Angle>(3, "Angle")?);
        let turn = |from: f32, to: f32| from + ((to - from + 180.0).rem_euclid(360.0) - 180.0) * t;
        Ok(Angle { p: turn(from.p, to.p), y: turn(from.y, to.y), r: turn(from.r, to.r) })
    })?;
    vector_methods(lua, &meta::meta_table(lua, "Vector", type_id::VECTOR)?)?;
    angle_methods(lua, &meta::meta_table(lua, "Angle", type_id::ANGLE)?)?;
    Ok(())
}

fn vector_methods(lua: &Lua, t: &Table) -> mlua::Result<()> {
    let vec = |args: &Args, position: usize| args.userdata::<Vector>(position, "Vector");

    def(lua, t, "Length", move |a| Ok(vec(a, 1)?.length()))?;
    def(lua, t, "LengthSqr", move |a| {
        let v = vec(a, 1)?;
        Ok(v.dot(v))
    })?;
    def(lua, t, "Length2D", move |a| {
        let v = vec(a, 1)?;
        Ok((v.x * v.x + v.y * v.y).sqrt())
    })?;
    def(lua, t, "Length2DSqr", move |a| {
        let v = vec(a, 1)?;
        Ok(v.x * v.x + v.y * v.y)
    })?;
    def(lua, t, "Distance", move |a| Ok((vec(a, 1)? - vec(a, 2)?).length()))?;
    def(lua, t, "DistToSqr", move |a| {
        let d = vec(a, 1)? - vec(a, 2)?;
        Ok(d.dot(d))
    })?;
    def(lua, t, "Distance2D", move |a| {
        let d = vec(a, 1)? - vec(a, 2)?;
        Ok((d.x * d.x + d.y * d.y).sqrt())
    })?;
    def(lua, t, "Distance2DSqr", move |a| {
        let d = vec(a, 1)? - vec(a, 2)?;
        Ok(d.x * d.x + d.y * d.y)
    })?;
    def(lua, t, "Dot", move |a| Ok(vec(a, 1)?.dot(vec(a, 2)?)))?;
    def(lua, t, "DotProduct", move |a| Ok(vec(a, 1)?.dot(vec(a, 2)?)))?;
    def(lua, t, "Cross", move |a| Ok(vec(a, 1)?.cross(vec(a, 2)?)))?;
    def(lua, t, "GetNormalized", move |a| Ok(vec(a, 1)?.normalized()))?;
    def(lua, t, "GetNormal", move |a| Ok(vec(a, 1)?.normalized()))?;
    def(lua, t, "Normalize", |a| {
        let mut v = this_vector(a)?;
        *v = v.normalized();
        Ok(())
    })?;
    def(lua, t, "GetNegated", move |a| Ok(-vec(a, 1)?))?;
    def(lua, t, "Negate", |a| {
        let mut v = this_vector(a)?;
        *v = -*v;
        Ok(())
    })?;
    def(lua, t, "Zero", |a| {
        *this_vector(a)? = Vector::default();
        Ok(())
    })?;
    def(lua, t, "IsZero", move |a| Ok(vec(a, 1)? == Vector::default()))?;
    def(lua, t, "Set", move |a| {
        let w = vec(a, 2)?;
        *this_vector(a)? = w;
        Ok(())
    })?;
    def(lua, t, "SetUnpacked", |a| {
        let w = Vector::new(f32_arg(a, 2)?, f32_arg(a, 3)?, f32_arg(a, 4)?);
        *this_vector(a)? = w;
        Ok(())
    })?;
    def(lua, t, "Unpack", move |a| {
        let v = vec(a, 1)?;
        Ok((v.x, v.y, v.z))
    })?;
    def(lua, t, "Add", move |a| {
        let w = vec(a, 2)?;
        let mut v = this_vector(a)?;
        *v = *v + w;
        Ok(())
    })?;
    def(lua, t, "Sub", move |a| {
        let w = vec(a, 2)?;
        let mut v = this_vector(a)?;
        *v = *v - w;
        Ok(())
    })?;
    def(lua, t, "Mul", |a| {
        let by = match a.get(2) {
            Some(Value::UserData(_)) => a.userdata::<Vector>(2, "Vector")?,
            _ => {
                let s = f32_arg(a, 2)?;
                Vector::new(s, s, s)
            }
        };
        let mut v = this_vector(a)?;
        *v = v.zip(by, |x, y| x * y);
        Ok(())
    })?;
    def(lua, t, "Div", |a| {
        let by = match a.get(2) {
            Some(Value::UserData(_)) => a.userdata::<Vector>(2, "Vector")?,
            _ => {
                let s = f32_arg(a, 2)?;
                Vector::new(s, s, s)
            }
        };
        let mut v = this_vector(a)?;
        *v = v.zip(by, |x, y| x / y);
        Ok(())
    })?;
    def(lua, t, "IsEqualTol", move |a| {
        let (v, w, tolerance) = (vec(a, 1)?, vec(a, 2)?, f32_arg(a, 3)?);
        Ok((v.x - w.x).abs() <= tolerance && (v.y - w.y).abs() <= tolerance && (v.z - w.z).abs() <= tolerance)
    })?;
    def(lua, t, "Angle", move |a| Ok(vec(a, 1)?.angle()))?;
    def(lua, t, "AngleEx", move |a| Ok(vec(a, 1)?.angle_with_up(vec(a, 2)?)))?;
    def(lua, t, "Rotate", move |a| {
        let angle = a.userdata::<Angle>(2, "Angle")?;
        let mut v = this_vector(a)?;
        *v = v.rotated(angle);
        Ok(())
    })?;
    def(lua, t, "WithinAABox", move |a| {
        let (v, lo, hi) = (vec(a, 1)?, vec(a, 2)?, vec(a, 3)?);
        let within = |c: f32, a: f32, b: f32| c >= a.min(b) && c <= a.max(b);
        Ok(within(v.x, lo.x, hi.x) && within(v.y, lo.y, hi.y) && within(v.z, lo.z, hi.z))
    })?;
    def(lua, t, "ToTable", move |a| {
        let v = vec(a, 1)?;
        a.lua.create_sequence_from([v.x, v.y, v.z])
    })?;
    def(lua, t, "ToColor", move |a| {
        let v = vec(a, 1)?;
        let color: mlua::Function = a.lua.globals().get("Color")?;
        color.call::<Value>((v.x * 255.0, v.y * 255.0, v.z * 255.0))
    })?;
    Ok(())
}

fn angle_methods(lua: &Lua, t: &Table) -> mlua::Result<()> {
    let ang = |args: &Args, position: usize| args.userdata::<Angle>(position, "Angle");

    def(lua, t, "Forward", move |a| Ok(ang(a, 1)?.vectors().0))?;
    def(lua, t, "Right", move |a| Ok(ang(a, 1)?.vectors().1))?;
    def(lua, t, "Up", move |a| Ok(ang(a, 1)?.vectors().2))?;
    def(lua, t, "Normalize", |a| {
        let mut angle = this_angle(a)?;
        *angle = angle.normalized();
        Ok(())
    })?;
    def(lua, t, "RotateAroundAxis", move |a| {
        let (axis, degrees) = (a.userdata::<Vector>(2, "Vector")?, f32_arg(a, 3)?);
        let mut angle = this_angle(a)?;
        *angle = angle.rotated_about(axis, degrees);
        Ok(())
    })?;
    def(lua, t, "Zero", |a| {
        *this_angle(a)? = Angle::default();
        Ok(())
    })?;
    def(lua, t, "IsZero", move |a| Ok(ang(a, 1)? == Angle::default()))?;
    def(lua, t, "Set", move |a| {
        let other = ang(a, 2)?;
        *this_angle(a)? = other;
        Ok(())
    })?;
    def(lua, t, "SetUnpacked", |a| {
        let other = Angle::new(f32_arg(a, 2)?, f32_arg(a, 3)?, f32_arg(a, 4)?);
        *this_angle(a)? = other;
        Ok(())
    })?;
    def(lua, t, "Unpack", move |a| {
        let angle = ang(a, 1)?;
        Ok((angle.p, angle.y, angle.r))
    })?;
    def(lua, t, "Add", move |a| {
        let other = ang(a, 2)?;
        let mut angle = this_angle(a)?;
        *angle = angle.zip(other, |x, y| x + y);
        Ok(())
    })?;
    def(lua, t, "Sub", move |a| {
        let other = ang(a, 2)?;
        let mut angle = this_angle(a)?;
        *angle = angle.zip(other, |x, y| x - y);
        Ok(())
    })?;
    def(lua, t, "Mul", |a| {
        let s = f32_arg(a, 2)?;
        let mut angle = this_angle(a)?;
        *angle = angle.map(|c| c * s);
        Ok(())
    })?;
    def(lua, t, "Div", |a| {
        let s = f32_arg(a, 2)?;
        let mut angle = this_angle(a)?;
        *angle = angle.map(|c| c / s);
        Ok(())
    })?;
    def(lua, t, "IsEqualTol", move |a| {
        let (x, y, tolerance) = (ang(a, 1)?, ang(a, 2)?, f32_arg(a, 3)?);
        let close = |a: f32, b: f32| normalize_degrees(a - b).abs() <= tolerance;
        Ok(close(x.p, y.p) && close(x.y, y.y) && close(x.r, y.r))
    })?;
    def(lua, t, "SnapTo", |a| {
        let which = a.string(2)?;
        let step = f32_arg(a, 3)?;
        let mut angle = this_angle(a)?;
        let snap = |c: f32| if step == 0.0 { c } else { (c / step).round() * step };
        match which.as_str() {
            "p" | "pitch" => angle.p = snap(angle.p),
            "y" | "yaw" => angle.y = snap(angle.y),
            "r" | "roll" => angle.r = snap(angle.r),
            _ => return Err(a.bad(2, "p, y or r")),
        }
        Ok(*angle)
    })?;
    def(lua, t, "ToTable", move |a| {
        let angle = ang(a, 1)?;
        a.lua.create_sequence_from([angle.p, angle.y, angle.r])
    })?;
    Ok(())
}
