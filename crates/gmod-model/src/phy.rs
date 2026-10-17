//! Physics models (`.phy`): what a model collides with, how heavy it is, and for ragdolls how
//! its parts hang together.
//!
//! A `.phy` starts with a 16-byte header - `i32` its size, `i32` 0, `i32` the number of solids,
//! `i32` the model's checksum - then each solid as an `i32` size and that many bytes of
//! collision data, and ends with text: blocks of key-value pairs (`solid`, `editparams`,
//! `ragdollconstraint`, `collisionrules` ...).
//!
//! A solid's collision data is IVP's compact surface, behind a 28-byte header in models
//! compiled since 2006 (`"VPHY"`, `i16` version, `i16` model type, 0 for polyhedra, `i32` size
//! of the surface, `f32`×3 drag areas, `i32` axis map size); older models start with the
//! surface itself. The surface (48 bytes: mass centre, inertia, radius, `i32` with 8 bits of
//! flags and 24 of its size, `i32` offset of the root of its tree of ledges, three `i32`, the
//! last `"IVPS"`) holds ledges - convex pieces - under a binary tree. The ledges at the leaves
//! of the tree make up the solid; an inner node may point to a ledge as well, the hull of
//! everything under it, which is no part of the shape.
//!
//! * A node (28 bytes): `i32` offset of its right child, 0 at a leaf (the left child is the
//!   next node), `i32` offset of its ledge, both from the node; centre, radius, box sizes.
//! * A ledge (16 bytes): `i32` offset of its points from the ledge, `i32` client data, `u32`
//!   with 2 bits "has children", 2 bits "compact", 4 unused and 24 bits of its size in 16-byte
//!   units, `i16` number of triangles, `i16` unused; its triangles follow.
//! * A triangle (16 bytes): a `u32` with its index (12 bits), pierce index (12), material (7)
//!   and a virtual flag (1); then 3 edges of a `u32` each: the index of its first point (16
//!   bits), the offset to the opposite edge (15, signed) and a virtual flag.
//! * A point: `f32`×3, then a fourth `f32` that is of no use here.
//!
//! IVP counts in metres, its axes turned: its point (x, y, z) is (x, z, −y) / 0.0254 in the
//! model's units. A solid lies in the space of its bone: the model's own for a prop of one
//! bone, the part's bone for a ragdoll.

use std::collections::HashMap;
use std::io;

use crate::reader::{Reader, invalid};

const VPHY_HEADER_SIZE: usize = 28;
const SURFACE_SIZE: usize = 48;
const NODE_SIZE: usize = 28;
const LEDGE_SIZE: usize = 16;
const TRIANGLE_SIZE: usize = 16;
const POINT_SIZE: usize = 16;
/// A unit of the model in metres, IVP's unit.
pub const METRES_PER_UNIT: f32 = 0.0254;

/// A point of IVP in the model's units and axes.
fn from_ivp(p: [f32; 3]) -> [f32; 3] {
    [p[0] / METRES_PER_UNIT, p[2] / METRES_PER_UNIT, -p[1] / METRES_PER_UNIT]
}

/// A model's point in IVP's metres and axes, as a `.phy` stores it.
pub fn to_ivp(p: [f32; 3]) -> [f32; 3] {
    [p[0] * METRES_PER_UNIT, -p[2] * METRES_PER_UNIT, p[1] * METRES_PER_UNIT]
}

/// A convex piece of a solid.
#[derive(Debug, Clone, PartialEq)]
pub struct Convex {
    /// The corners of its hull, in the model's units.
    pub points: Vec<[f32; 3]>,
    /// The hull's triangles, as indices of `points`: counter-clockwise seen from outside, so
    /// that `(b - a) × (c - a)` points out.
    pub triangles: Vec<[u16; 3]>,
}

/// A block of the text: `name { "key" "value" ... }`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    pub pairs: Vec<(String, String)>,
}

impl Block {
    /// The first value of `key` (ASCII case ignored).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }

    /// A value that is a number.
    pub fn number(&self, key: &str) -> Option<f32> {
        self.get(key)?.trim().parse().ok()
    }
}

/// How a solid's mass lies, as its compact surface was built with it: where its mass is, in
/// the model's units, and its inertia about each of the model's axes per kilogram (kg·m²) -
/// which VPhysics turns it by, not that of a solid of its shape (IVP's own reckoning: of a
/// box, about x, the square root of the sum of the squares of its second moments along y
/// and z, not their sum). And how much of its box each side of it meets of the air, 0 to 1,
/// by the axis it faces along.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MassShape {
    pub centre: [f32; 3],
    pub inertia: [f32; 3],
    pub drag_areas: Option<[f32; 3]>,
}

/// One solid: the shape of a prop, or of one part of a ragdoll.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Solid {
    pub convexes: Vec<Convex>,
    /// How its mass lies, as its collision data says.
    pub mass_shape: Option<MassShape>,
    /// Its `solid` block of the text: `name` (its bone), `mass`, `surfaceprop`, `damping`,
    /// `rotdamping`, `inertia`, `volume` ...; empty when the text has none for it.
    pub keys: Block,
}

impl Solid {
    /// Its bone's name.
    pub fn name(&self) -> &str {
        self.keys.get("name").unwrap_or("")
    }

    /// Kilograms.
    pub fn mass(&self) -> Option<f32> {
        self.keys.number("mass")
    }

    /// The surface property: what it sounds and breaks like (`scripts/surfaceproperties*.txt`).
    pub fn surface_prop(&self) -> Option<&str> {
        self.keys.get("surfaceprop")
    }
}

/// A joint of a ragdoll: how far a part turns about its parent, in degrees about each axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RagdollConstraint {
    /// Solids, by number.
    pub parent: usize,
    pub child: usize,
    /// For x, y and z: the least and the most.
    pub limits: [[f32; 2]; 3],
    pub friction: [f32; 3],
}

/// A physics model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Phy {
    /// The checksum of the model it was compiled with.
    pub checksum: i32,
    pub solids: Vec<Solid>,
    /// The blocks of the text other than the solids', in order.
    pub blocks: Vec<Block>,
}

impl Phy {
    pub fn parse(data: &[u8]) -> io::Result<Phy> {
        let r = Reader::new(data, "physics model");
        let header = r.count(0, "header byte")?;
        if header < 16 {
            return Err(invalid(format!("not a physics model (.phy): a header of {header} bytes")));
        }
        let solid_count = r.count(8, "solid")?;
        let checksum = r.i32(12)?;
        // every solid takes at least its size
        r.table(header, solid_count, 4, "solids")?;
        let mut at = header;
        let mut solids = Vec::with_capacity(solid_count);
        for i in 0..solid_count {
            let size = r.count(at, "solid byte")?;
            let bytes = r.bytes(at + 4, size)?;
            let convexes = convexes(bytes).map_err(|e| invalid(format!("solid {i}: {e}")))?;
            solids.push(Solid { convexes, mass_shape: mass_shape(bytes), keys: Block::default() });
            at += 4 + size;
        }
        let mut blocks = Vec::new();
        for block in parse_blocks(&String::from_utf8_lossy(&data[at..])) {
            let solid = block.name.eq_ignore_ascii_case("solid").then(|| block.number("index")).flatten();
            match solid.filter(|i| *i >= 0.0).and_then(|i| solids.get_mut(i as usize)) {
                Some(solid) => solid.keys = block,
                None => blocks.push(block),
            }
        }
        Ok(Phy { checksum, solids, blocks })
    }

    /// The first block of the text named `name` (`editparams` ...), solids aside.
    pub fn block(&self, name: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.name.eq_ignore_ascii_case(name))
    }

    /// The joints of a ragdoll.
    pub fn ragdoll_constraints(&self) -> Vec<RagdollConstraint> {
        self.blocks
            .iter()
            .filter(|b| b.name.eq_ignore_ascii_case("ragdollconstraint"))
            .filter_map(|b| {
                let solid = |key: &str| b.number(key).filter(|n| *n >= 0.0).map(|n| n as usize);
                let axis = |a: &str| [b.number(&format!("{a}min")).unwrap_or(0.0), b.number(&format!("{a}max")).unwrap_or(0.0)];
                let friction = |a: &str| b.number(&format!("{a}friction")).unwrap_or(0.0);
                Some(RagdollConstraint {
                    parent: solid("parent")?,
                    child: solid("child")?,
                    limits: [axis("x"), axis("y"), axis("z")],
                    friction: [friction("x"), friction("y"), friction("z")],
                })
            })
            .collect()
    }

    /// The only parts of a ragdoll that hit each other, by solid, when its `collisionrules`
    /// says: the pairs it names (`"collisionpair" "2,5"`), none when it says
    /// `"selfcollisions" "0"`. None without rules: then every part hits every other, but for
    /// those joined.
    pub fn collision_pairs(&self) -> Option<Vec<[usize; 2]>> {
        let rules = self.block("collisionrules")?;
        if rules.number("selfcollisions") == Some(0.0) {
            return Some(Vec::new());
        }
        let solid = |s: &str| s.trim().parse::<usize>().ok().filter(|&i| i < self.solids.len());
        let pairs = rules.pairs.iter().filter(|(key, _)| key.eq_ignore_ascii_case("collisionpair")).filter_map(|(_, value)| {
            let (a, b) = value.split_once(',')?;
            Some([solid(a)?, solid(b)?])
        });
        Some(pairs.collect())
    }
}

/// The mass centre, inertia and drag areas of one solid's collision data (its surface's first
/// 24 bytes, and the header before it): IVP's axes turned to the model's.
fn mass_shape(data: &[u8]) -> Option<MassShape> {
    let r = Reader::new(data, "collision data");
    let header = data.starts_with(b"VPHY");
    let surface = if header { VPHY_HEADER_SIZE } else { 0 };
    let three = |at: usize| -> Option<[f32; 3]> { Some([r.f32(at).ok()?, r.f32(at + 4).ok()?, r.f32(at + 8).ok()?]) };
    let centre = from_ivp(three(surface)?);
    let [x, y, z] = three(surface + 12)?;
    // IVP's (x, y, z) is the model's (x, z, -y): its inertia about y is about the model's z
    let inertia = [x, z, y];
    // the drag areas are in the model's axes already
    let drag_areas = if header { three(12) } else { None };
    // a surface built without them (none of its inertia) says nothing
    let known = inertia.iter().all(|i| i.is_finite() && *i >= 0.0) && inertia.iter().any(|&i| i > 0.0);
    (known && centre.iter().all(|c| c.is_finite())).then_some(MassShape { centre, inertia, drag_areas })
}

/// The convex pieces of one solid's collision data.
fn convexes(data: &[u8]) -> io::Result<Vec<Convex>> {
    let r = Reader::new(data, "collision data");
    // without the header, an old model's surface starts at once
    let surface = if data.starts_with(b"VPHY") {
        match r.i16(6)? {
            0 => VPHY_HEADER_SIZE,
            other => return Err(invalid(format!("collision model of type {other}: only polyhedra (0) are known"))),
        }
    } else {
        0
    };
    r.bytes(surface, SURFACE_SIZE)?;
    let root = r.offset(surface, surface + 32)?;
    let mut out = Vec::new();
    // the leaves of the tree, left to right; every step goes on in the data, so it ends
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        r.bytes(node, NODE_SIZE)?;
        match r.i32(node)? {
            0 => {
                let convex = ledge(&r, r.offset(node, node + 4)?)?;
                if !convex.triangles.is_empty() {
                    out.push(convex);
                }
            }
            right if right > 0 => {
                stack.push(node + right as usize);
                stack.push(node + NODE_SIZE);
            }
            right => return Err(invalid(format!("a node of the ledge tree at {node} points back by {}", -right))),
        }
    }
    Ok(out)
}

/// A ledge: its triangles, and the points they use.
fn ledge(r: &Reader, at: usize) -> io::Result<Convex> {
    r.bytes(at, LEDGE_SIZE)?;
    let points_at = r.offset(at, at)?;
    let count = usize::try_from(r.i16(at + 12)?).map_err(|_| invalid(format!("the ledge at {at} has a negative number of triangles")))?;
    r.table(at + LEDGE_SIZE, count, TRIANGLE_SIZE, "triangles")?;
    // the points of the solid are shared by its ledges: each ledge keeps those it uses
    let mut used: Vec<u16> = Vec::new();
    let mut numbered: HashMap<u16, u16> = HashMap::new();
    let mut triangles = Vec::with_capacity(count);
    for t in 0..count {
        let edges = at + LEDGE_SIZE + t * TRIANGLE_SIZE + 4;
        let mut corners = [0u16; 3];
        for (corner, edge) in corners.iter_mut().zip(0..3) {
            let index = r.u16(edges + edge * 4)?;
            *corner = *numbered.entry(index).or_insert_with(|| {
                used.push(index);
                (used.len() - 1) as u16
            });
        }
        triangles.push(corners);
    }
    let points = used
        .iter()
        .map(|&index| {
            let p = points_at + usize::from(index) * POINT_SIZE;
            r.bytes(p, POINT_SIZE).map_err(|_| invalid(format!("the ledge at {at} uses point {index}, past the end")))?;
            Ok(from_ivp(r.vec3(p)?))
        })
        .collect::<io::Result<Vec<[f32; 3]>>>()?;
    // facing out: away from the middle of the points
    let n = points.len().max(1) as f32;
    let middle = points.iter().fold([0.0f32; 3], |s, p| [s[0] + p[0] / n, s[1] + p[1] / n, s[2] + p[2] / n]);
    for tri in &mut triangles {
        let [a, b, c] = tri.map(|i| points[usize::from(i)]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let inward = [middle[0] - a[0], middle[1] - a[1], middle[2] - a[2]];
        if normal[0] * inward[0] + normal[1] * inward[1] + normal[2] * inward[2] > 0.0 {
            tri.swap(1, 2);
        }
    }
    Ok(Convex { points, triangles })
}

fn token<'a>(text: &'a str, pos: &mut usize) -> Option<&'a str> {
    let bytes = text.as_bytes();
    while *pos < bytes.len() && (bytes[*pos].is_ascii_whitespace() || bytes[*pos] == 0) {
        *pos += 1;
    }
    let start = *pos;
    match bytes.get(start)? {
        b'{' | b'}' => {
            *pos += 1;
            Some(&text[start..start + 1])
        }
        b'"' => {
            let end = text[start + 1..].find('"').map_or(text.len(), |e| start + 1 + e);
            *pos = (end + 1).min(text.len());
            Some(&text[start + 1..end])
        }
        _ => {
            let end = text[start..]
                .find(|c: char| c.is_ascii_whitespace() || c == '"' || c == '{' || c == '}' || c == '\0')
                .map_or(text.len(), |e| start + e);
            *pos = end;
            Some(&text[start..end])
        }
    }
}

/// The blocks of the text. What does not fit the pattern is passed over; a block inside a
/// block is skipped.
fn parse_blocks(text: &str) -> Vec<Block> {
    let mut pos = 0;
    let mut blocks = Vec::new();
    while let Some(name) = token(text, &mut pos) {
        if name == "{" || name == "}" {
            continue;
        }
        let mut probe = pos;
        if token(text, &mut probe) != Some("{") {
            continue;
        }
        pos = probe;
        let mut block = Block { name: name.to_string(), pairs: Vec::new() };
        loop {
            match token(text, &mut pos) {
                None | Some("}") => break,
                Some("{") => {
                    let mut depth = 1;
                    while depth > 0 {
                        match token(text, &mut pos) {
                            None => break,
                            Some("{") => depth += 1,
                            Some("}") => depth -= 1,
                            Some(_) => {}
                        }
                    }
                }
                Some(key) => {
                    let mut probe = pos;
                    match token(text, &mut probe) {
                        // a key whose value is a block: skipped with it
                        Some("{") => {}
                        Some("}") | None => {}
                        Some(value) => {
                            pos = probe;
                            block.pairs.push((key.to_string(), value.to_string()));
                        }
                    }
                }
            }
        }
        blocks.push(block);
    }
    blocks
}
