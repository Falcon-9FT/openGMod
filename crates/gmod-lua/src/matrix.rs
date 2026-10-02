//! `VMatrix`, GMod's 4x4 matrix, and `Matrix`, which makes one.
//!
//! As Source's `VMatrix`, it holds 32-bit floats by rows: `m[row][column]`. The first three
//! columns of the 3x3 part are the axes of the space it turns into - forward, left, up (so
//! `GetRight` is the second column negated) - and the fourth column is the translation. A
//! vector times a matrix is a point: turned, then moved.

use mlua::{AnyUserData, IntoLuaMulti, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::{self, Args};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub m: [[f32; 4]; 4],
}

impl Default for Matrix {
    fn default() -> Matrix {
        Matrix::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { m: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]] };
    pub const ZERO: Matrix = Matrix { m: [[0.0; 4]; 4] };

    fn column(&self, c: usize) -> Vector {
        Vector::new(self.m[0][c], self.m[1][c], self.m[2][c])
    }

    fn set_column(&mut self, c: usize, v: Vector) {
        self.m[0][c] = v.x;
        self.m[1][c] = v.y;
        self.m[2][c] = v.z;
    }

    pub fn forward(&self) -> Vector {
        self.column(0)
    }

    pub fn right(&self) -> Vector {
        -self.column(1)
    }

    pub fn up(&self) -> Vector {
        self.column(2)
    }

    pub fn translation(&self) -> Vector {
        self.column(3)
    }

    pub fn set_translation(&mut self, v: Vector) {
        self.set_column(3, v);
    }

    /// A rotation, as Source's `AngleMatrix`: the axes of the angle as columns.
    pub fn from_angle(angle: Angle) -> Matrix {
        let mut out = Matrix::IDENTITY;
        out.set_angles(angle);
        out
    }

    pub fn from_translation(v: Vector) -> Matrix {
        let mut out = Matrix::IDENTITY;
        out.set_translation(v);
        out
    }

    pub fn from_scale(v: Vector) -> Matrix {
        let mut out = Matrix::IDENTITY;
        out.m[0][0] = v.x;
        out.m[1][1] = v.y;
        out.m[2][2] = v.z;
        out
    }

    /// The 3x3 part becomes the rotation of `angle`; the translation stays.
    pub fn set_angles(&mut self, angle: Angle) {
        let (forward, right, up) = angle.vectors();
        self.set_column(0, forward);
        self.set_column(1, -right);
        self.set_column(2, up);
    }

    /// Source's `MatrixAngles`: the angle of the 3x3 part.
    pub fn angles(&self) -> Angle {
        let (forward, left, up) = (self.column(0), self.column(1), self.column(2));
        let horizontal = (forward.x * forward.x + forward.y * forward.y).sqrt();
        if horizontal > 0.001 {
            Angle::new(
                (-forward.z).atan2(horizontal).to_degrees(),
                forward.y.atan2(forward.x).to_degrees(),
                left.z.atan2(up.z).to_degrees(),
            )
        } else {
            // looking straight up or down: the yaw is in the left axis, there is no roll
            Angle::new((-forward.z).atan2(horizontal).to_degrees(), (-left.x).atan2(left.y).to_degrees(), 0.0)
        }
    }

    /// The length of each of the three axes.
    pub fn scale(&self) -> Vector {
        Vector::new(self.column(0).length(), self.column(1).length(), self.column(2).length())
    }

    /// The axes brought to the lengths of `scale`, keeping their directions.
    pub fn set_scale(&mut self, scale: Vector) {
        for (c, length) in [scale.x, scale.y, scale.z].into_iter().enumerate() {
            self.set_column(c, self.column(c).normalized() * length);
        }
    }

    pub fn transposed(&self) -> Matrix {
        let mut out = Matrix::ZERO;
        for (r, row) in out.m.iter_mut().enumerate() {
            for (c, value) in row.iter_mut().enumerate() {
                *value = self.m[c][r];
            }
        }
        out
    }

    fn zip(&self, other: &Matrix, f: impl Fn(f32, f32) -> f32) -> Matrix {
        let mut out = *self;
        for (row, other_row) in out.m.iter_mut().zip(&other.m) {
            for (value, other_value) in row.iter_mut().zip(other_row) {
                *value = f(*value, *other_value);
            }
        }
        out
    }

    /// The point `v` (w = 1) carried through the matrix.
    pub fn transform(&self, v: Vector) -> Vector {
        let row = |r: [f32; 4]| r[0] * v.x + r[1] * v.y + r[2] * v.z + r[3];
        Vector::new(row(self.m[0]), row(self.m[1]), row(self.m[2]))
    }

    /// The general inverse by Gauss-Jordan elimination; none when the matrix is singular.
    pub fn inverse(&self) -> Option<Matrix> {
        let mut a = self.m.map(|row| row.map(f64::from));
        let mut inv = Matrix::IDENTITY.m.map(|row| row.map(f64::from));
        for col in 0..4 {
            let pivot = (col..4).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
            if a[pivot][col].abs() < 1e-8 {
                return None;
            }
            a.swap(col, pivot);
            inv.swap(col, pivot);
            let scale = 1.0 / a[col][col];
            for c in 0..4 {
                a[col][c] *= scale;
                inv[col][c] *= scale;
            }
            for r in 0..4 {
                if r != col {
                    let factor = a[r][col];
                    for c in 0..4 {
                        a[r][c] -= factor * a[col][c];
                        inv[r][c] -= factor * inv[col][c];
                    }
                }
            }
        }
        Some(Matrix { m: inv.map(|row| row.map(|v| v as f32)) })
    }

    /// The inverse of a rotation and a translation, as Source's `MatrixInverseTR`: the
    /// rotation transposed, the translation turned back. Wrong for anything scaled.
    pub fn inverse_tr(&self) -> Matrix {
        let mut out = Matrix::IDENTITY;
        for r in 0..3 {
            for c in 0..3 {
                out.m[r][c] = self.m[c][r];
            }
        }
        let t = self.translation();
        let row = |r: usize| -(out.m[r][0] * t.x + out.m[r][1] * t.y + out.m[r][2] * t.z);
        out.set_translation(Vector::new(row(0), row(1), row(2)));
        out
    }

    /// Source's `IsRotationMatrix`: three axes of length 1 at right angles to each other.
    pub fn is_rotation(&self) -> bool {
        let (x, y, z) = (self.column(0), self.column(1), self.column(2));
        let unit = |v: Vector| (v.dot(v) - 1.0).abs() < 0.01;
        let square = |a: Vector, b: Vector| a.dot(b).abs() < 0.01;
        unit(x) && unit(y) && unit(z) && square(x, y) && square(y, z) && square(x, z)
    }
}

impl std::ops::Mul for Matrix {
    type Output = Matrix;
    fn mul(self, o: Matrix) -> Matrix {
        let mut out = Matrix::ZERO;
        for (r, row) in out.m.iter_mut().enumerate() {
            for (c, value) in row.iter_mut().enumerate() {
                *value = (0..4).map(|k| self.m[r][k] * o.m[k][c]).sum();
            }
        }
        out
    }
}

fn matrix_of(value: &Value) -> Option<Matrix> {
    match value {
        Value::UserData(ud) => ud.borrow::<Matrix>().ok().map(|m| *m),
        _ => None,
    }
}

fn vector_of(value: &Value) -> Option<Vector> {
    match value {
        Value::UserData(ud) => ud.borrow::<Vector>().ok().map(|v| *v),
        _ => None,
    }
}

fn operand_error(lua: &Lua, op: &str, a: &Value, b: &Value) -> mlua::Error {
    args::error(lua, format!("attempt to perform arithmetic ({op}) on {} and {}", args::type_name(Some(a)), args::type_name(Some(b))))
}

impl UserData for Matrix {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "VMatrix");
        fields.add_meta_field("MetaID", type_id::MATRIX);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "VMatrix", key));
        methods.add_meta_function("__mul", |lua, (a, b): (Value, Value)| match (matrix_of(&a), matrix_of(&b), vector_of(&b)) {
            (Some(x), Some(y), _) => Ok(Value::UserData(lua.create_userdata(x * y)?)),
            (Some(x), None, Some(v)) => Ok(Value::UserData(lua.create_userdata(x.transform(v))?)),
            _ => Err(operand_error(lua, "mul", &a, &b)),
        });
        methods.add_meta_function("__add", |lua, (a, b): (Value, Value)| match (matrix_of(&a), matrix_of(&b)) {
            (Some(x), Some(y)) => Ok(x.zip(&y, |p, q| p + q)),
            _ => Err(operand_error(lua, "add", &a, &b)),
        });
        methods.add_meta_function("__sub", |lua, (a, b): (Value, Value)| match (matrix_of(&a), matrix_of(&b)) {
            (Some(x), Some(y)) => Ok(x.zip(&y, |p, q| p - q)),
            _ => Err(operand_error(lua, "sub", &a, &b)),
        });
        methods.add_meta_function("__unm", |_, m: AnyUserData| Ok(m.borrow::<Matrix>()?.zip(&Matrix::ZERO, |p, _| -p)));
        methods.add_meta_function("__eq", |_, (a, b): (Value, Value)| Ok(matrix_of(&a).is_some() && matrix_of(&a) == matrix_of(&b)));
        methods.add_meta_function("__tostring", |_, m: AnyUserData| {
            let m = *m.borrow::<Matrix>()?;
            let rows: Vec<String> = m.m.iter().map(|r| format!("[{:.6},\t{:.6},\t{:.6},\t{:.6}]", r[0], r[1], r[2], r[3])).collect();
            Ok(rows.join("\n"))
        });
    }
}

/// Puts a method into the named metatable.
fn def<R: IntoLuaMulti>(lua: &Lua, table: &Table, name: &'static str, f: impl Fn(&Args) -> mlua::Result<R> + 'static) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn mat(args: &Args, position: usize) -> mlua::Result<Matrix> {
    args.userdata::<Matrix>(position, "VMatrix")
}

/// Changes the matrix a method was called on.
fn change(args: &Args, f: impl FnOnce(&mut Matrix)) -> mlua::Result<()> {
    match args.get(1) {
        Some(Value::UserData(ud)) => {
            let mut m = ud.borrow_mut::<Matrix>().map_err(|_| args.bad(1, "VMatrix"))?;
            f(&mut m);
            Ok(())
        }
        _ => Err(args.bad(1, "VMatrix")),
    }
}

/// A row or column from 1 to 4, as an index.
fn index(args: &Args, position: usize) -> mlua::Result<usize> {
    let n = args.number(position)?;
    if (1.0..5.0).contains(&n) {
        Ok(n as usize - 1)
    } else {
        Err(args::error(args.lua, format!("bad argument #{position} to '{}' (1 to 4 expected)", args.name)))
    }
}

/// The matrix of `Matrix(data)`: four rows of four numbers, rows or numbers left out staying
/// those of the identity.
fn from_table(table: &Table) -> mlua::Result<Matrix> {
    let mut out = Matrix::IDENTITY;
    for (r, row) in out.m.iter_mut().enumerate() {
        if let Some(values) = table.raw_get::<Option<Table>>(r + 1)? {
            for (c, value) in row.iter_mut().enumerate() {
                if let Some(n) = values.raw_get::<Option<f64>>(c + 1)? {
                    *value = n as f32;
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().raw_set(
        "Matrix",
        lua.create_function(|lua, values: MultiValue| {
            let args = Args::new(lua, values, "Matrix");
            match args.get(1) {
                None => Ok(Matrix::IDENTITY),
                Some(Value::Table(t)) => from_table(t),
                Some(_) => mat(&args, 1).map_err(|_| args.bad(1, "table")),
            }
        })?,
    )?;

    let t = meta::meta_table(lua, "VMatrix", type_id::MATRIX)?;
    let t = &t;
    def(lua, t, "GetField", |a| {
        let (row, column) = (index(a, 2)?, index(a, 3)?);
        Ok(mat(a, 1)?.m[row][column])
    })?;
    def(lua, t, "SetField", |a| {
        let (row, column, value) = (index(a, 2)?, index(a, 3)?, a.number(4)? as f32);
        change(a, |m| m.m[row][column] = value)
    })?;
    def(lua, t, "GetForward", |a| Ok(mat(a, 1)?.forward()))?;
    def(lua, t, "GetRight", |a| Ok(mat(a, 1)?.right()))?;
    def(lua, t, "GetUp", |a| Ok(mat(a, 1)?.up()))?;
    def(lua, t, "GetTranslation", |a| Ok(mat(a, 1)?.translation()))?;
    def(lua, t, "GetAngles", |a| Ok(mat(a, 1)?.angles()))?;
    def(lua, t, "GetScale", |a| Ok(mat(a, 1)?.scale()))?;
    def(lua, t, "SetForward", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| m.set_column(0, v))
    })?;
    def(lua, t, "SetRight", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| m.set_column(1, -v))
    })?;
    def(lua, t, "SetUp", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| m.set_column(2, v))
    })?;
    def(lua, t, "SetTranslation", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| m.set_translation(v))
    })?;
    def(lua, t, "SetAngles", |a| {
        let angle = a.userdata::<Angle>(2, "Angle")?;
        change(a, |m| m.set_angles(angle))
    })?;
    def(lua, t, "SetScale", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| m.set_scale(v))
    })?;
    def(lua, t, "Translate", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| *m = *m * Matrix::from_translation(v))
    })?;
    def(lua, t, "Rotate", |a| {
        let angle = a.userdata::<Angle>(2, "Angle")?;
        change(a, |m| *m = *m * Matrix::from_angle(angle))
    })?;
    def(lua, t, "Scale", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        change(a, |m| *m = *m * Matrix::from_scale(v))
    })?;
    def(lua, t, "ScaleTranslation", |a| {
        let s = a.number(2)? as f32;
        change(a, |m| m.set_translation(m.translation() * s))
    })?;
    def(lua, t, "Set", |a| {
        let other = mat(a, 2)?;
        change(a, |m| *m = other)
    })?;
    def(lua, t, "Add", |a| {
        let other = mat(a, 2)?;
        change(a, |m| *m = m.zip(&other, |p, q| p + q))
    })?;
    def(lua, t, "Sub", |a| {
        let other = mat(a, 2)?;
        change(a, |m| *m = m.zip(&other, |p, q| p - q))
    })?;
    def(lua, t, "Mul", |a| {
        let other = mat(a, 2)?;
        change(a, |m| *m = *m * other)
    })?;
    def(lua, t, "Identity", |a| change(a, |m| *m = Matrix::IDENTITY))?;
    def(lua, t, "Zero", |a| change(a, |m| *m = Matrix::ZERO))?;
    def(lua, t, "IsIdentity", |a| Ok(mat(a, 1)? == Matrix::IDENTITY))?;
    def(lua, t, "IsZero", |a| Ok(mat(a, 1)? == Matrix::ZERO))?;
    def(lua, t, "IsRotationMatrix", |a| Ok(mat(a, 1)?.is_rotation()))?;
    def(lua, t, "GetTransposed", |a| Ok(mat(a, 1)?.transposed()))?;
    def(lua, t, "GetInverse", |a| Ok(mat(a, 1)?.inverse()))?;
    def(lua, t, "GetInverseTR", |a| Ok(mat(a, 1)?.inverse_tr()))?;
    def(lua, t, "Invert", |a| {
        let Some(inverse) = mat(a, 1)?.inverse() else { return Ok(false) };
        change(a, |m| *m = inverse)?;
        Ok(true)
    })?;
    def(lua, t, "InvertTR", |a| change(a, |m| *m = m.inverse_tr()))?;
    def(lua, t, "ToTable", |a| {
        let m = mat(a, 1)?;
        let rows = a.lua.create_table()?;
        for (r, row) in m.m.iter().enumerate() {
            rows.raw_set(r + 1, a.lua.create_sequence_from(*row)?)?;
        }
        Ok(rows)
    })?;
    def(lua, t, "Unpack", |a| Ok(MultiValue::from_iter(mat(a, 1)?.m.as_flattened().iter().map(|&v| Value::Number(v.into())))))?;
    def(lua, t, "SetUnpacked", |a| {
        let mut values = [0.0; 16];
        for (i, value) in values.iter_mut().enumerate() {
            *value = a.number(i + 2)? as f32;
        }
        change(a, |m| m.m.as_flattened_mut().copy_from_slice(&values))
    })?;
    Ok(())
}
