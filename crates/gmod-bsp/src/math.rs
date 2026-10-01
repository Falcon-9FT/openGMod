//! Vectors of three `f32`s, as the map's records hold them.

pub(crate) type Vec3 = [f32; 3];

pub(crate) fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub(crate) fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub(crate) fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

/// `a` made one long; straight up when it has no length.
pub(crate) fn normalize(a: Vec3) -> Vec3 {
    let len = length(a);
    if len > 1e-12 { scale(a, 1.0 / len) } else { [0.0, 0.0, 1.0] }
}

pub(crate) fn lerp3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

pub(crate) fn abs(a: Vec3) -> Vec3 {
    a.map(f32::abs)
}
