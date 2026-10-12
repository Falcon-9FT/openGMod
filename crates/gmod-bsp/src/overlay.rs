//! Overlays (`info_overlay`): pictures laid over faces of the world - signs, cracks, road
//! markings.
//!
//! An overlay is a four-cornered picture in the plane through its origin across its normal.
//! The compiler keeps its U axis in the third numbers of its first three points (they lie in
//! the plane and need none); V is the normal across U. Corner `i` is `origin + U x + V y` of
//! point `i`, and the texture runs from the start to the end of U and V: corner 0 has
//! `(u0, v0)`, 1 `(u0, v1)`, 2 `(u1, v1)`, 3 `(u1, v0)`.
//!
//! It shows on the faces it names only: the picture is carried along its normal onto each
//! face's plane and cut to the face's edges ([`on_face`]). [`crate::mesh`] lifts it a little
//! off the face, so that the two do not fight over depth, and lights it by the face's light
//! map where it lies. Displacements, which overlays follow in the game, are left out.

use crate::lumps::Overlay;
use crate::math::{Vec3, add, cross, dot, length, normalize, scale, sub};
use crate::{Bsp, Face};

/// How far an overlay is lifted off its face, in units: far less than can be seen, far more
/// than the depth buffer needs to tell them apart.
pub const LIFT: f32 = 0.1;

/// A point of an overlay and its texture coordinates.
pub type Point = (Vec3, [f32; 2]);

impl Overlay {
    /// Its U and V axes. An overlay without a U axis (a compiler that kept none) gets one
    /// across its normal.
    pub fn axes(&self) -> (Vec3, Vec3) {
        let normal = normalize(self.normal);
        let mut u = [self.points[0][2], self.points[1][2], self.points[2][2]];
        if length(u) < 0.5 {
            let other = if normal[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
            u = cross(other, normal);
        }
        let u = normalize(u);
        (u, normalize(cross(normal, u)))
    }

    /// Its four corners in the world, with their texture coordinates.
    pub fn corners(&self) -> [Point; 4] {
        let (u, v) = self.axes();
        let coords = [[self.u[0], self.v[0]], [self.u[0], self.v[1]], [self.u[1], self.v[1]], [self.u[1], self.v[0]]];
        std::array::from_fn(|i| {
            let [x, y, _] = self.points[i];
            (add(self.origin, add(scale(u, x), scale(v, y))), coords[i])
        })
    }
}

/// The point where the line from `a` to `b` crosses a plane they are `da` and `db` in front of.
fn cut((pa, ta): Point, (pb, tb): Point, da: f32, db: f32) -> Point {
    let t = da / (da - db);
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    ([lerp(pa[0], pb[0]), lerp(pa[1], pb[1]), lerp(pa[2], pb[2])], [lerp(ta[0], tb[0]), lerp(ta[1], tb[1])])
}

/// What is left of a polygon on the inner side of a plane (`dot(normal, p) >= dist`).
fn clip(polygon: &[Point], normal: Vec3, dist: f32) -> Vec<Point> {
    let mut out = Vec::with_capacity(polygon.len() + 1);
    for (i, &a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        let (da, db) = (dot(normal, a.0) - dist, dot(normal, b.0) - dist);
        if da >= 0.0 {
            out.push(a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            out.push(cut(a, b, da, db));
        }
    }
    out
}

/// The part of an overlay on one face, in its plane, wound as faces are (clockwise from the
/// front); empty when they do not meet.
pub fn on_face(bsp: &Bsp, overlay: &Overlay, face: &Face) -> Vec<Point> {
    let Some(plane) = bsp.planes.get(face.plane as usize) else { return Vec::new() };
    let n = plane.normal;
    let along = normalize(overlay.normal);
    // an overlay seen edge on from the face cannot be carried onto it
    let facing = dot(n, along);
    if facing.abs() < 0.1 {
        return Vec::new();
    }
    let mut polygon: Vec<Point> =
        overlay.corners().iter().map(|&(p, uv)| (add(p, scale(along, (plane.dist - dot(n, p)) / facing)), uv)).collect();
    let corners: Vec<Vec3> = bsp.face_vertices(face).collect();
    if corners.len() < 3 {
        return Vec::new();
    }
    let middle = scale(corners.iter().fold([0.0; 3], |sum, &c| add(sum, c)), 1.0 / corners.len() as f32);
    for (i, &a) in corners.iter().enumerate() {
        let b = corners[(i + 1) % corners.len()];
        // a vertex written twice makes an edge with no direction, which would clip anywhere
        if length(sub(b, a)) < 1e-3 {
            continue;
        }
        let mut inward = normalize(cross(n, sub(b, a)));
        if dot(inward, sub(middle, a)) < 0.0 {
            inward = scale(inward, -1.0);
        }
        // a hair outside the edge still counts, so that an overlay up to it keeps its edge
        polygon = clip(&polygon, inward, dot(inward, a) - 0.01);
        if polygon.len() < 3 {
            return Vec::new();
        }
    }
    // faces turn clockwise seen from the front: the normal is the second edge across the first
    let turn =
        (2..polygon.len()).fold([0.0; 3], |sum, i| add(sum, cross(sub(polygon[i].0, polygon[0].0), sub(polygon[i - 1].0, polygon[0].0))));
    if dot(turn, n) < 0.0 {
        polygon.reverse();
    }
    polygon
}
