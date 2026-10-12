//! What is solid in a map, and sweeps through it: how the engine moves a player's box, and how
//! it answers `util.TraceLine` and `util.TraceHull`.
//!
//! * The world and every brush entity have a BSP tree: nodes cut space by planes down to
//!   leaves, and a leaf lists the brushes that reach into it. A brush is a convex solid, the
//!   space behind the planes of all its sides.
//! * A box is swept as a point through brushes grown by the box: each plane is pushed out by
//!   how far the box reaches toward it. The bevels the compiler adds to brushes make the grown
//!   brush fit at edges and corners too, so that a box does not stop short of an edge that is
//!   not there.
//! * A sweep stops [`EPSILON`] short of what it hits, as the engine's do, so that the next
//!   sweep from there does not start inside it.
//! * A displacement is a surface, not a solid: it stops what comes at it from the front. Its
//!   triangles are grown by the box along every axis that can keep a box apart from them.
//! * Brush entities stand where the map puts them, turned by their angles. A turned one is
//!   swept with a box grown to hold the box turned the other way. Those nothing collides with
//!   (triggers, `func_illusionary`, clips for physics objects ...) are left out.
//! * Models that do not move - static props - collide by convex hulls ([`Hull`]): those of their
//!   physics models, which the caller reads ([`Collision::add_shape`],
//!   [`Collision::add_static`]). A box keeps the world's axes against a turned prop: the hull is
//!   grown by it along the hull's faces, the box's faces and every crossing of an edge of one
//!   with an edge of the other, which is exact.

use crate::lumps::contents;
use crate::math::{Vec3, abs, add, cross, dot, length, lerp3, normalize, scale, sub};
use crate::{Bsp, Entity, mesh};

/// How far short of what it hits a sweep stops, in units.
pub const EPSILON: f32 = 1.0 / 32.0;

/// What a sweep stops at: the [`contents`] that count as solid for it (`MASK_` in GMod's Lua).
pub mod mask {
    use crate::lumps::contents as c;

    pub const ALL: u32 = 0xFFFF_FFFF;
    /// What stops everything: `util.TraceLine` and `util.TraceHull` without a mask.
    pub const SOLID: u32 = c::SOLID | c::MOVEABLE | c::WINDOW | c::MONSTER | c::GRATE;
    /// What stops players: the solid and the clips for players.
    pub const PLAYERSOLID: u32 = SOLID | c::PLAYERCLIP;
    pub const NPCSOLID: u32 = c::SOLID | c::MOVEABLE | c::MONSTERCLIP | c::WINDOW | c::MONSTER | c::GRATE;
    pub const SOLID_BRUSHONLY: u32 = c::SOLID | c::MOVEABLE | c::WINDOW | c::GRATE;
    pub const PLAYERSOLID_BRUSHONLY: u32 = SOLID_BRUSHONLY | c::PLAYERCLIP;
    pub const WATER: u32 = c::WATER | c::MOVEABLE | c::SLIME;
    pub const OPAQUE: u32 = c::SOLID | c::MOVEABLE | c::OPAQUE;
    pub const VISIBLE: u32 = OPAQUE | c::IGNORE_NODRAW_OPAQUE;
    /// What stops bullets.
    pub const SHOT: u32 = c::SOLID | c::MOVEABLE | c::MONSTER | c::WINDOW | c::DEBRIS | c::HITBOX;
}

/// The surface of a side: its `texinfo` and the [`surf`](crate::surf) flags of that.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Surface {
    texinfo: Option<u32>,
    flags: i32,
}

/// A plane that bounds a solid, its normal facing out.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Bound {
    normal: Vec3,
    dist: f32,
    /// Only boxes need it: it adds nothing to a point.
    bevel: bool,
    surface: Surface,
}

#[derive(Debug, Clone, Copy)]
struct TreeNode {
    normal: Vec3,
    dist: f32,
    children: [i32; 2],
}

#[derive(Debug, Clone, Copy)]
struct TreeLeaf {
    contents: u32,
    first_brush: u32,
    brushes: u32,
}

#[derive(Debug, Clone, Copy)]
struct Solid {
    contents: u32,
    first_side: u32,
    sides: u32,
}

#[derive(Debug, Clone, Copy)]
struct Triangle {
    points: [Vec3; 3],
    /// Facing the front of the displacement.
    normal: Vec3,
    mins: Vec3,
    maxs: Vec3,
}

#[derive(Debug, Clone)]
struct Displacement {
    contents: u32,
    surface: Surface,
    mins: Vec3,
    maxs: Vec3,
    triangles: Vec<Triangle>,
}

/// The axes of something turned by pitch, yaw and roll (degrees): forward, left and up, as
/// the engine turns models.
fn axes(angles: Vec3) -> [Vec3; 3] {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let ((sp, cp), (sy, cy), (sr, cr)) = (pitch.sin_cos(), yaw.sin_cos(), roll.sin_cos());
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

/// Where something stands: moved to an origin, and turned.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Frame {
    origin: Vec3,
    /// Its axes in the world; `None` when it is not turned.
    axes: Option<[Vec3; 3]>,
}

impl Frame {
    fn new(origin: Vec3, angles: Vec3) -> Frame {
        Frame { origin, axes: (angles != [0.0; 3]).then(|| axes(angles)) }
    }

    fn local_point(&self, p: Vec3) -> Vec3 {
        let d = sub(p, self.origin);
        match self.axes {
            Some([f, l, u]) => [dot(f, d), dot(l, d), dot(u, d)],
            None => d,
        }
    }

    fn world_direction(&self, v: Vec3) -> Vec3 {
        match self.axes {
            Some([f, l, u]) => add(add(scale(f, v[0]), scale(l, v[1])), scale(u, v[2])),
            None => v,
        }
    }

    fn world_point(&self, p: Vec3) -> Vec3 {
        add(self.origin, self.world_direction(p))
    }

    /// The half size, in its space, of a box that holds a box of half size `e` in the world.
    fn local_extents(&self, e: Vec3) -> Vec3 {
        match self.axes {
            Some([f, l, u]) => [dot(abs(f), e), dot(abs(l), e), dot(abs(u), e)],
            None => e,
        }
    }

    /// The world's axes in its space: how a box of the world lies there.
    fn world_axes(&self) -> Option<[Vec3; 3]> {
        self.axes.map(|[f, l, u]| [[f[0], l[0], u[0]], [f[1], l[1], u[1]], [f[2], l[2], u[2]]])
    }
}

/// A brush model where it stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub model: usize,
    /// Its entity, in [`Bsp::entities`]; `None` for the world.
    pub entity: Option<usize>,
    pub origin: Vec3,
    /// Pitch, yaw and roll, in degrees.
    pub angles: Vec3,
    /// Its entity may move or go ([`crate::Entity::moves`]): it is not part of the world's
    /// solid shapes, and [`Collision::place_model`] moves it.
    pub moves: bool,
    frame: Frame,
}

impl Placement {
    pub fn new(model: usize, entity: Option<usize>, origin: Vec3, angles: Vec3) -> Placement {
        Placement { model, entity, origin, angles, moves: false, frame: Frame::new(origin, angles) }
    }
}

/// A convex solid of a model, in the model's space: its corners, the planes of its faces and
/// the directions of its edges.
#[derive(Debug, Clone, PartialEq)]
pub struct Hull {
    points: Vec<Vec3>,
    /// Normals facing out, and distances.
    faces: Vec<(Vec3, f32)>,
    edges: Vec<Vec3>,
    mins: Vec3,
    maxs: Vec3,
}

impl Hull {
    /// The hull with the corners `points` and the faces `triangles`, wound either way; `None`
    /// when it has no volume.
    pub fn new(points: &[Vec3], triangles: &[[usize; 3]]) -> Option<Hull> {
        if points.len() < 4 || points.iter().flatten().any(|v| !v.is_finite()) {
            return None;
        }
        let middle = scale(points.iter().fold([0.0; 3], |m, &p| add(m, p)), 1.0 / points.len() as f32);
        let (mut faces, mut edges): (Vec<(Vec3, f32)>, Vec<Vec3>) = (Vec::new(), Vec::new());
        for tri in triangles {
            let [Some(&a), Some(&b), Some(&c)] = tri.map(|i| points.get(i)) else { continue };
            let n = cross(sub(b, a), sub(c, a));
            if length(n) < 1e-6 {
                continue;
            }
            // facing away from the middle
            let n = scale(n, if dot(n, sub(middle, a)) > 0.0 { -1.0 } else { 1.0 } / length(n));
            let dist = dot(n, a);
            if !faces.iter().any(|&(m, d)| dot(m, n) > 1.0 - 1e-5 && (d - dist).abs() < 1e-3) {
                faces.push((n, dist));
            }
            for (p, q) in [(a, b), (b, c), (c, a)] {
                let e = sub(q, p);
                if length(e) > 1e-6 {
                    let e = normalize(e);
                    if !edges.iter().any(|&f| dot(f, e).abs() > 1.0 - 1e-5) {
                        edges.push(e);
                    }
                }
            }
        }
        // a solid has four faces at least, and its middle inside all of them
        if faces.len() < 4 || faces.iter().any(|&(n, d)| dot(n, middle) >= d) {
            return None;
        }
        let mins = std::array::from_fn(|i| points.iter().map(|p| p[i]).fold(f32::MAX, f32::min));
        let maxs = std::array::from_fn(|i| points.iter().map(|p| p[i]).fold(f32::MIN, f32::max));
        Some(Hull { points: points.to_vec(), faces, edges, mins, maxs })
    }

    /// A box.
    pub fn cuboid(mins: Vec3, maxs: Vec3) -> Hull {
        let points: Vec<Vec3> =
            (0..8).map(|i| std::array::from_fn(|axis| if i & (1 << axis) == 0 { mins[axis] } else { maxs[axis] })).collect();
        let faces = (0..3)
            .flat_map(|axis| {
                let mut n = [0.0; 3];
                n[axis] = 1.0;
                [(n, maxs[axis]), (scale(n, -1.0), -mins[axis])]
            })
            .collect();
        Hull { points, faces, edges: vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], mins, maxs }
    }

    /// How far it reaches along `n`.
    fn support(&self, n: Vec3) -> f32 {
        self.points.iter().map(|&p| dot(n, p)).fold(f32::MIN, f32::max)
    }

    /// The planes that bound it grown by the sweep's box: its faces, and for a box also the
    /// box's faces and every crossing of one of its edges with one of the box's - each axis
    /// that can keep the two apart.
    fn bounds(&self, sweep: &Sweep) -> Vec<Bound> {
        let mut out: Vec<Bound> = self.faces.iter().map(|&(normal, dist)| Bound { normal, dist, ..Bound::default() }).collect();
        if sweep.point {
            return out;
        }
        let axes = sweep.axes.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        let mut both_ways = |n: Vec3| {
            for normal in [n, scale(n, -1.0)] {
                out.push(Bound { normal, dist: self.support(normal), bevel: true, surface: Surface::default() });
            }
        };
        for axis in axes {
            both_ways(axis);
            for &edge in &self.edges {
                let c = cross(edge, axis);
                if length(c) > 1e-6 {
                    both_ways(normalize(c));
                }
            }
        }
        out
    }
}

/// A model that does not move.
#[derive(Debug, Clone)]
struct Static {
    shape: usize,
    /// Its number among the map's static props.
    prop: usize,
    frame: Frame,
    /// Its bounds in the world.
    mins: Vec3,
    maxs: Vec3,
}

/// What is solid in a map as shapes for a physics engine, in the world's space.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SolidShapes {
    /// Convex solids by their corners: each brush, and each hull of a static prop.
    pub hulls: Vec<Vec<Vec3>>,
    /// The displacements' triangles, as indices of `vertices`.
    pub vertices: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
}

/// The corners of the solid behind all of `sides`: where three of their planes meet, inside the
/// rest.
fn corners(sides: &[Bound]) -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::new();
    for (i, a) in sides.iter().enumerate() {
        for (j, b) in sides.iter().enumerate().skip(i + 1) {
            for c in &sides[j + 1..] {
                let det = dot(a.normal, cross(b.normal, c.normal));
                if det.abs() < 1e-6 {
                    continue;
                }
                let sum = add(
                    add(scale(cross(b.normal, c.normal), a.dist), scale(cross(c.normal, a.normal), b.dist)),
                    scale(cross(a.normal, b.normal), c.dist),
                );
                let p = scale(sum, 1.0 / det);
                let inside = sides.iter().all(|s| dot(s.normal, p) <= s.dist + 0.01);
                if inside && !out.iter().any(|&q| length(sub(p, q)) < 0.01) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// What a sweep found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trace {
    /// How far it got: 0 at the start, 1 at the end.
    pub fraction: f32,
    /// Where it got to.
    pub end: Vec3,
    /// The normal of the plane it hit, facing back at it; zero when it hit nothing.
    pub normal: Vec3,
    /// It started inside something solid.
    pub start_solid: bool,
    /// It was inside something solid all the way; then it did not move.
    pub all_solid: bool,
    /// When it started inside: how far it got before it was out.
    pub fraction_left_solid: f32,
    /// The [`contents`] of what it hit.
    pub contents: u32,
    /// The surface it hit: its `texinfo` in the map, and the [`surf`](crate::surf) flags of that.
    pub texinfo: Option<usize>,
    pub surface_flags: i32,
    /// What it hit is a displacement.
    pub displacement: bool,
    /// The brush model it hit, 0 being the world.
    pub model: Option<usize>,
    /// The static prop it hit, by its number in the map.
    pub static_prop: Option<usize>,
}

impl Trace {
    /// It hit something, or started inside something.
    pub fn hit(&self) -> bool {
        self.fraction < 1.0 || self.start_solid
    }
}

/// What a sweep through one model hit, in the model's space.
#[derive(Debug, Clone, Copy)]
struct Hit {
    normal: Vec3,
    contents: u32,
    surface: Surface,
    displacement: bool,
}

/// A box (a point when it has no size) swept through one model, in the model's space.
struct Sweep {
    /// The box's middle.
    start: Vec3,
    end: Vec3,
    /// Its half size along its axes.
    extents: Vec3,
    /// Its axes, when they are not the model's: a box of the world in a turned model's space.
    axes: Option<[Vec3; 3]>,
    point: bool,
    mask: u32,
    /// The nearest hit so far, through any model.
    fraction: f32,
    /// A nearer hit in this model.
    hit: Option<Hit>,
    start_solid: bool,
    all_solid: bool,
    left_solid: f32,
}

impl Sweep {
    /// How far the box reaches along `normal`.
    fn reach(&self, normal: Vec3) -> f32 {
        match (self.point, self.axes) {
            (true, _) => 0.0,
            (false, None) => dot(abs(normal), self.extents),
            (false, Some(axes)) => (0..3).map(|i| self.extents[i] * dot(normal, axes[i]).abs()).sum(),
        }
    }

    /// Sweeps against a convex solid bounded by `bounds`. A thin solid - a triangle swept by a
    /// ray - is hit even where going in and coming out are the same moment.
    fn clip(&mut self, bounds: impl Iterator<Item = Bound>, contents: u32, displacement: bool, thin: bool) {
        let (mut enter, mut leave) = (-1.0f32, 1.0f32);
        let mut entered = None;
        let (mut starts_out, mut ends_out) = (false, false);
        for bound in bounds {
            if bound.bevel && self.point {
                continue;
            }
            let reach = self.reach(bound.normal);
            let d1 = dot(bound.normal, self.start) - bound.dist - reach;
            let d2 = dot(bound.normal, self.end) - bound.dist - reach;
            starts_out |= d1 > 0.0;
            ends_out |= d2 > 0.0;
            // in front of this side all the way, or moving away from it: never inside
            if d1 > 0.0 && (d2 >= EPSILON || d2 >= d1) {
                return;
            }
            // behind it all the way: the other sides decide
            if d1 <= 0.0 && d2 <= 0.0 {
                continue;
            }
            if d1 > d2 {
                // going in through this side: stop short of it
                let f = (d1 - EPSILON) / (d1 - d2);
                if f > enter {
                    enter = f;
                    entered = Some(bound);
                }
            } else {
                // coming out through it
                leave = leave.min((d1 + EPSILON) / (d1 - d2));
            }
        }
        if !starts_out {
            self.start_solid = true;
            if ends_out {
                self.left_solid = self.left_solid.max(leave);
            } else {
                self.all_solid = true;
                self.fraction = 0.0;
                self.hit = Some(Hit { normal: [0.0; 3], contents, surface: Surface::default(), displacement });
            }
            return;
        }
        let touches = if thin { enter <= leave } else { enter < leave };
        if let Some(bound) = entered
            && touches
            && enter > -1.0
            && enter < self.fraction
        {
            self.fraction = enter.max(0.0);
            self.hit = Some(Hit { normal: bound.normal, contents, surface: bound.surface, displacement });
        }
    }

    /// The space the sweep passes through, grown by `margin`.
    fn bounds(&self, margin: f32) -> (Vec3, Vec3) {
        let grow = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|axis| self.reach(axis) + margin);
        let lo = std::array::from_fn(|i| self.start[i].min(self.end[i]) - grow[i]);
        let hi = std::array::from_fn(|i| self.start[i].max(self.end[i]) + grow[i]);
        (lo, hi)
    }
}

fn overlap(lo: Vec3, hi: Vec3, mins: Vec3, maxs: Vec3) -> bool {
    (0..3).all(|i| lo[i] <= maxs[i] && hi[i] >= mins[i])
}

/// The planes that bound a triangle grown by a box: its two faces, and every other axis that
/// can keep the two apart - the box's faces and the crossings of their edges. A point needs
/// only the faces and the triangle's own edges. Up to 26 planes; returns them and how many.
fn triangle_bounds(tri: &Triangle, point: bool, surface: Surface) -> ([Bound; 26], usize) {
    let mut out = [Bound::default(); 26];
    let mut n = 0;
    let mut both_ways = |normal: Vec3| {
        let along = tri.points.map(|p| dot(normal, p));
        let (lo, hi) = (along[0].min(along[1]).min(along[2]), along[0].max(along[1]).max(along[2]));
        out[n] = Bound { normal, dist: hi, bevel: false, surface };
        out[n + 1] = Bound { normal: scale(normal, -1.0), dist: -lo, bevel: false, surface };
        n += 2;
    };
    const AXES: [Vec3; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    both_ways(tri.normal);
    if !point {
        AXES.into_iter().for_each(&mut both_ways);
    }
    let [a, b, c] = tri.points;
    let mut crossed = |e: Vec3, with: Vec3| {
        let m = cross(e, with);
        if length(m) > 1e-6 {
            both_ways(normalize(m));
        }
    };
    for e in [sub(b, a), sub(c, b), sub(a, c)] {
        if point {
            crossed(e, tri.normal);
        } else {
            AXES.into_iter().for_each(|axis| crossed(e, axis));
        }
    }
    (out, n)
}

/// Brush entities nothing collides with: volumes for the game's logic and its effects, and
/// clips that stop only physics objects or vehicles.
const NOT_SOLID: [&str; 14] = [
    "trigger_",
    "func_illusionary",
    "func_areaportal",
    "func_occluder",
    "func_viscluster",
    "func_dustmotes",
    "func_dustcloud",
    "func_smokevolume",
    "func_precipitation",
    "func_respawnroom",
    "func_regenerate",
    "func_nobuild",
    "func_clip_vphysics",
    "func_vehicleclip",
];

/// Whether a brush entity is never solid, whatever is done to it.
fn never_solid(entity: &Entity) -> bool {
    let class = entity.class_name();
    NOT_SOLID.iter().any(|not| class.starts_with(not)) || class == "func_brush" && entity.get("Solidity") == Some("1")
}

/// Whether a brush entity is solid when the map starts.
fn solid_at_start(entity: &Entity) -> bool {
    let class = entity.class_name();
    if NOT_SOLID.iter().any(|not| class.starts_with(not)) {
        return false;
    }
    match class {
        // "Solidity": 0 goes with the brush being on, 1 never solid, 2 always
        "func_brush" => match entity.get("Solidity") {
            Some("1") => false,
            Some("2") => true,
            _ => entity.get("StartDisabled") != Some("1"),
        },
        // spawn flag 1: starts invisible and not solid
        "func_wall_toggle" => entity.get("spawnflags").and_then(|f| f.parse::<i64>().ok()).is_none_or(|f| f & 1 == 0),
        _ => true,
    }
}

/// What is solid in a map, ready for sweeps.
#[derive(Debug, Clone, Default)]
pub struct Collision {
    nodes: Vec<TreeNode>,
    leaves: Vec<TreeLeaf>,
    leaf_brushes: Vec<u16>,
    brushes: Vec<Solid>,
    sides: Vec<Bound>,
    displacements: Vec<Displacement>,
    /// Each brush model's head node; empty when the map has no trees.
    heads: Vec<i32>,
    /// Each brush model's bounds in its own space, from its brushes; `None` when a brush has
    /// no side facing along some axis to tell.
    model_bounds: Vec<Option<(Vec3, Vec3)>>,
    /// The world and the brush entities that are solid where the map puts them.
    pub placements: Vec<Placement>,
    /// The brush entities that may move and are not solid now: off, or not yet on.
    parked: Vec<Placement>,
    /// The convex hulls of models, in their own space.
    shapes: Vec<Vec<Hull>>,
    statics: Vec<Static>,
    /// The world's bounds.
    pub mins: Vec3,
    pub maxs: Vec3,
}

impl Collision {
    /// Gathers what is solid in a map: the world, its displacements and the brush entities
    /// that are solid when the map starts.
    pub fn new(bsp: &Bsp) -> Collision {
        let surface = |texinfo: i16| {
            let found = usize::try_from(texinfo).ok().and_then(|i| bsp.texinfo.get(i).map(|info| (i, info)));
            Surface { texinfo: found.map(|(i, _)| i as u32), flags: found.map_or(0, |(_, info)| info.flags) }
        };
        let nodes = bsp
            .nodes
            .iter()
            .map(|n| {
                let plane = bsp.planes[n.plane as usize];
                TreeNode { normal: plane.normal, dist: plane.dist, children: n.children }
            })
            .collect();
        let leaves = bsp
            .leaves
            .iter()
            .map(|l| TreeLeaf { contents: l.contents, first_brush: l.first_leaf_brush.into(), brushes: l.num_leaf_brushes.into() })
            .collect();
        let sides = bsp
            .brush_sides
            .iter()
            .map(|s| {
                let plane = bsp.planes[s.plane as usize];
                Bound { normal: plane.normal, dist: plane.dist, bevel: s.bevel, surface: surface(s.texinfo) }
            })
            .collect();
        let brushes = bsp
            .brushes
            .iter()
            .map(|b| Solid { contents: b.contents, first_side: b.first_side as u32, sides: b.num_sides as u32 })
            .collect();
        let heads = if bsp.nodes.is_empty() { Vec::new() } else { bsp.models.iter().map(|m| m.head_node).collect() };
        let (mins, maxs) = bsp.models.first().map_or(([0.0; 3], [0.0; 3]), |m| (m.mins, m.maxs));
        let mut collision = Collision {
            nodes,
            leaves,
            leaf_brushes: bsp.leaf_brushes.clone(),
            brushes,
            sides,
            displacements: displacements(bsp, &surface),
            heads,
            model_bounds: Vec::new(),
            placements: placements(bsp, true),
            parked: placements(bsp, false),
            shapes: Vec::new(),
            statics: Vec::new(),
            mins,
            maxs,
        };
        collision.model_bounds = (0..collision.heads.len()).map(|m| collision.brush_bounds(m)).collect();
        collision
    }

    /// The map has no world to sweep through: no trees (a map cut down to be drawn only).
    pub fn is_empty(&self) -> bool {
        self.heads.is_empty()
    }

    /// Adds the convex hulls of a model - a shape for [`Collision::add_static`] - and returns
    /// its number.
    pub fn add_shape(&mut self, hulls: Vec<Hull>) -> usize {
        self.shapes.push(hulls);
        self.shapes.len() - 1
    }

    /// Adds a model that does not move: the map's static prop number `prop`, of the shape
    /// numbered `shape`, at `origin` and turned by `angles`. A shape that was not added is
    /// nothing.
    pub fn add_static(&mut self, prop: usize, shape: usize, origin: Vec3, angles: Vec3) {
        let Some(hulls) = self.shapes.get(shape) else { return };
        let frame = Frame::new(origin, angles);
        let (mut mins, mut maxs) = ([f32::MAX; 3], [f32::MIN; 3]);
        for p in hulls.iter().flat_map(|h| &h.points) {
            let p = frame.world_point(*p);
            mins = std::array::from_fn(|i| mins[i].min(p[i]));
            maxs = std::array::from_fn(|i| maxs[i].max(p[i]));
        }
        self.statics.push(Static { shape, prop, frame, mins, maxs });
    }

    /// What is solid for `mask` as shapes for a physics engine: the brushes of the world and
    /// of the solid brush entities where the map puts them, the static props' hulls and the
    /// displacements' triangles.
    pub fn solid_shapes(&self, mask: u32) -> SolidShapes {
        let mut out = SolidShapes::default();
        for placement in self.placements.iter().filter(|p| !p.moves) {
            let Some(&head) = self.heads.get(placement.model) else { continue };
            let mut seen = vec![false; self.brushes.len()];
            let mut stack = vec![head];
            while let Some(node) = stack.pop() {
                let Ok(index) = usize::try_from(node) else {
                    let leaf = self.leaves[(-1 - node) as usize];
                    for &b in &self.leaf_brushes[leaf.first_brush as usize..(leaf.first_brush + leaf.brushes) as usize] {
                        let brush = self.brushes[b as usize];
                        if std::mem::replace(&mut seen[b as usize], true) || brush.contents & mask == 0 {
                            continue;
                        }
                        let corners = corners(&self.sides[brush.first_side as usize..(brush.first_side + brush.sides) as usize]);
                        if corners.len() >= 4 {
                            out.hulls.push(corners.into_iter().map(|p| placement.frame.world_point(p)).collect());
                        }
                    }
                    continue;
                };
                stack.extend(self.nodes[index].children);
            }
        }
        for displacement in self.displacements.iter().filter(|d| d.contents & mask != 0) {
            for triangle in &displacement.triangles {
                let first = out.vertices.len() as u32;
                out.vertices.extend(triangle.points);
                out.triangles.push([first, first + 1, first + 2]);
            }
        }
        for prop in &self.statics {
            for hull in self.shapes.get(prop.shape).into_iter().flatten() {
                out.hulls.push(hull.points.iter().map(|&p| prop.frame.world_point(p)).collect());
            }
        }
        out
    }

    /// Sweeps a box - `mins` to `maxs` around the point that moves - from `start` to `end`
    /// through the world and the solid brush entities, stopping at what `mask` names. A box
    /// without size is a ray.
    pub fn trace(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, mask: u32) -> Trace {
        let mut found = Found::default();
        for placement in &self.placements {
            self.sweep_placed(placement, start, end, mins, maxs, mask, &mut found);
        }
        for s in &self.statics {
            self.sweep_static(s, start, end, mins, maxs, mask, &mut found);
        }
        found.trace(start, end)
    }

    /// A ray from `start` to `end`.
    pub fn trace_line(&self, start: Vec3, end: Vec3, mask: u32) -> Trace {
        self.trace(start, end, [0.0; 3], [0.0; 3], mask)
    }

    /// The same as [`Collision::trace`], through one brush model alone, standing where
    /// `placement` puts it.
    pub fn trace_placed(&self, placement: &Placement, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, mask: u32) -> Trace {
        let mut found = Found::default();
        self.sweep_placed(placement, start, end, mins, maxs, mask, &mut found);
        found.trace(start, end)
    }

    /// Puts a brush model that may move where its entity is now (`Some(origin, angles)`), or
    /// takes it out of what is solid (`None`): a door that opens, a wall turned off or removed.
    /// Whether the model is one that may move.
    pub fn place_model(&mut self, model: usize, at: Option<(Vec3, Vec3)>) -> bool {
        let active = self.placements.iter().position(|p| p.moves && p.model == model);
        let parked = self.parked.iter().position(|p| p.model == model);
        match (at, active, parked) {
            (Some((origin, angles)), Some(i), _) => {
                let p = &mut self.placements[i];
                if p.origin != origin || p.angles != angles {
                    (p.origin, p.angles, p.frame) = (origin, angles, Frame::new(origin, angles));
                }
            }
            (Some((origin, angles)), None, Some(i)) => {
                let mut p = self.parked.swap_remove(i);
                (p.origin, p.angles, p.frame) = (origin, angles, Frame::new(origin, angles));
                self.placements.push(p);
            }
            (None, Some(i), _) => {
                let p = self.placements.remove(i);
                self.parked.push(p);
            }
            (None, None, Some(_)) => {}
            (_, None, None) => return false,
        }
        true
    }

    /// The convex brushes of one brush model that `mask` names, as their corners in the
    /// model's own space.
    pub fn model_hulls(&self, model: usize, mask: u32) -> Vec<Vec<Vec3>> {
        let mut out = Vec::new();
        let Some(&head) = self.heads.get(model) else { return out };
        let mut seen = vec![false; self.brushes.len()];
        let mut stack = vec![head];
        while let Some(node) = stack.pop() {
            let Ok(index) = usize::try_from(node) else {
                let Some(leaf) = self.leaves.get((-1 - node) as usize) else { continue };
                let range = leaf.first_brush as usize..(leaf.first_brush + leaf.brushes) as usize;
                for &b in self.leaf_brushes.get(range).unwrap_or(&[]) {
                    let Some(brush) = self.brushes.get(b as usize) else { continue };
                    if std::mem::replace(&mut seen[b as usize], true) || brush.contents & mask == 0 {
                        continue;
                    }
                    let corners = corners(&self.sides[brush.first_side as usize..(brush.first_side + brush.sides) as usize]);
                    if corners.len() >= 4 {
                        out.push(corners);
                    }
                }
                continue;
            };
            if let Some(n) = self.nodes.get(index) {
                stack.extend(n.children);
            }
        }
        out
    }

    /// The leaf of the world's tree a point is in: its index in the map's leaves.
    pub fn leaf_at(&self, point: Vec3) -> Option<usize> {
        let mut node = *self.heads.first()?;
        while let Ok(i) = usize::try_from(node) {
            let n = self.nodes.get(i)?;
            node = n.children[usize::from(dot(n.normal, point) < n.dist)];
        }
        Some((-1 - node) as usize).filter(|&leaf| leaf < self.leaves.len())
    }

    /// What is at a point: the [`contents`] of all the brushes there, of the world and of the
    /// solid brush entities. Static props are left out, as the engine does.
    pub fn point_contents(&self, point: Vec3) -> u32 {
        self.placements.iter().fold(contents::EMPTY, |c, placement| c | self.model_contents(placement, point))
    }

    fn model_contents(&self, placement: &Placement, point: Vec3) -> u32 {
        let Some(&head) = self.heads.get(placement.model) else { return contents::EMPTY };
        let p = placement.frame.local_point(point);
        let mut node = head;
        while let Ok(i) = usize::try_from(node) {
            let n = self.nodes[i];
            node = n.children[usize::from(dot(n.normal, p) < n.dist)];
        }
        let leaf = self.leaves[(-1 - node) as usize];
        if leaf.brushes == 0 {
            return leaf.contents;
        }
        let mut found = contents::EMPTY;
        for &b in &self.leaf_brushes[leaf.first_brush as usize..(leaf.first_brush + leaf.brushes) as usize] {
            let brush = self.brushes[b as usize];
            let sides = &self.sides[brush.first_side as usize..(brush.first_side + brush.sides) as usize];
            if !sides.is_empty() && sides.iter().all(|s| dot(s.normal, p) <= s.dist) {
                found |= brush.contents;
            }
        }
        found
    }

    #[allow(clippy::too_many_arguments)]
    fn sweep_placed(&self, placement: &Placement, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, mask: u32, found: &mut Found) {
        let Some(&head) = self.heads.get(placement.model) else { return };
        let offset = scale(add(mins, maxs), 0.5);
        let extents = abs(scale(sub(maxs, mins), 0.5));
        let mut sweep = Sweep {
            start: placement.frame.local_point(add(start, offset)),
            end: placement.frame.local_point(add(end, offset)),
            extents: placement.frame.local_extents(extents),
            axes: None,
            point: extents == [0.0; 3],
            mask,
            fraction: found.fraction,
            hit: None,
            start_solid: false,
            all_solid: false,
            left_solid: 0.0,
        };
        if placement.model != 0
            && let Some(Some((lo, hi))) = self.model_bounds.get(placement.model)
        {
            let (sweep_lo, sweep_hi) = sweep.bounds(1.0);
            if !overlap(sweep_lo, sweep_hi, *lo, *hi) {
                return;
            }
        }
        self.descend(head, &mut sweep);
        if placement.model == 0 {
            self.sweep_displacements(&mut sweep);
        }
        found.take(&sweep, &placement.frame, Some(placement.model), None);
    }

    #[allow(clippy::too_many_arguments)]
    fn sweep_static(&self, s: &Static, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, mask: u32, found: &mut Found) {
        if mask & contents::SOLID == 0 {
            return;
        }
        let offset = scale(add(mins, maxs), 0.5);
        let extents = abs(scale(sub(maxs, mins), 0.5));
        let (from, to) = (add(start, offset), add(end, offset));
        let lo = std::array::from_fn(|i| from[i].min(to[i]) - extents[i] - 1.0);
        let hi = std::array::from_fn(|i| from[i].max(to[i]) + extents[i] + 1.0);
        if !overlap(lo, hi, s.mins, s.maxs) {
            return;
        }
        // in the prop's space, the box keeps the world's axes
        let mut sweep = Sweep {
            start: s.frame.local_point(from),
            end: s.frame.local_point(to),
            extents,
            axes: s.frame.world_axes(),
            point: extents == [0.0; 3],
            mask,
            fraction: found.fraction,
            hit: None,
            start_solid: false,
            all_solid: false,
            left_solid: 0.0,
        };
        let (lo, hi) = sweep.bounds(1.0);
        for hull in &self.shapes[s.shape] {
            if overlap(lo, hi, hull.mins, hull.maxs) {
                sweep.clip(hull.bounds(&sweep).into_iter(), contents::SOLID, false, false);
            }
        }
        found.take(&sweep, &s.frame, None, Some(s.prop));
    }

    /// Walks the tree from `head` down the leaves the sweep passes through, the near ones
    /// first, and sweeps against their brushes.
    fn descend(&self, head: i32, sweep: &mut Sweep) {
        // the part of the sweep in a subtree: node, fractions, points
        let mut stack = vec![(head, 0.0f32, 1.0f32, sweep.start, sweep.end)];
        while let Some((node, f1, f2, p1, p2)) = stack.pop() {
            // all of it beyond what was hit already
            if f1 > sweep.fraction {
                continue;
            }
            let Ok(index) = usize::try_from(node) else {
                self.sweep_leaf((-1 - node) as usize, sweep);
                continue;
            };
            let n = self.nodes[index];
            let reach = sweep.reach(n.normal) + EPSILON;
            let d1 = dot(n.normal, p1) - n.dist;
            let d2 = dot(n.normal, p2) - n.dist;
            let [front, back] = n.children;
            if d1 >= reach && d2 >= reach {
                stack.push((front, f1, f2, p1, p2));
                continue;
            }
            if d1 <= -reach && d2 <= -reach {
                stack.push((back, f1, f2, p1, p2));
                continue;
            }
            // it crosses the plane or runs near it: the part reaching in front, the part
            // reaching behind
            let delta = d2 - d1;
            let (front_part, back_part) = if delta == 0.0 {
                ((0.0, 1.0), (0.0, 1.0))
            } else {
                let at_plus = ((reach - d1) / delta).clamp(0.0, 1.0);
                let at_minus = ((-reach - d1) / delta).clamp(0.0, 1.0);
                if delta > 0.0 { ((at_minus, 1.0), (0.0, at_plus)) } else { ((0.0, at_minus), (at_plus, 1.0)) }
            };
            let near_front = d1 >= 0.0;
            let parts = if near_front { [(back, back_part), (front, front_part)] } else { [(front, front_part), (back, back_part)] };
            // the far part goes on the stack first, so that the near one is taken first
            for (child, (a, b)) in parts {
                if a <= b {
                    stack.push((child, f1 + (f2 - f1) * a, f1 + (f2 - f1) * b, lerp3(p1, p2, a), lerp3(p1, p2, b)));
                }
            }
        }
    }

    fn sweep_leaf(&self, leaf: usize, sweep: &mut Sweep) {
        let leaf = self.leaves[leaf];
        for &b in &self.leaf_brushes[leaf.first_brush as usize..(leaf.first_brush + leaf.brushes) as usize] {
            let brush = self.brushes[b as usize];
            if brush.contents & sweep.mask == 0 || brush.sides == 0 {
                continue;
            }
            let sides = &self.sides[brush.first_side as usize..(brush.first_side + brush.sides) as usize];
            sweep.clip(sides.iter().copied(), brush.contents, false, false);
        }
    }

    fn sweep_displacements(&self, sweep: &mut Sweep) {
        let (lo, hi) = sweep.bounds(1.0);
        for disp in &self.displacements {
            if disp.contents & sweep.mask == 0 || !overlap(lo, hi, disp.mins, disp.maxs) {
                continue;
            }
            for tri in &disp.triangles {
                // only what comes at it from the front
                if !overlap(lo, hi, tri.mins, tri.maxs) || dot(tri.normal, sweep.start) < dot(tri.normal, tri.points[0]) {
                    continue;
                }
                let (bounds, count) = triangle_bounds(tri, sweep.point, disp.surface);
                sweep.clip(bounds[..count].iter().copied(), disp.contents, true, true);
            }
        }
    }

    /// The bounds of a brush model's brushes, from their sides that face along the axes (the
    /// compiler gives every brush those, as bevels where they are no faces).
    fn brush_bounds(&self, model: usize) -> Option<(Vec3, Vec3)> {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        let mut stack = vec![self.heads[model]];
        while let Some(node) = stack.pop() {
            let Ok(index) = usize::try_from(node) else {
                let leaf = self.leaves[(-1 - node) as usize];
                for &b in &self.leaf_brushes[leaf.first_brush as usize..(leaf.first_brush + leaf.brushes) as usize] {
                    let brush = self.brushes[b as usize];
                    let sides = &self.sides[brush.first_side as usize..(brush.first_side + brush.sides) as usize];
                    for axis in 0..3 {
                        let along = |sign: f32| sides.iter().find(|s| (0..3).all(|i| s.normal[i] == if i == axis { sign } else { 0.0 }));
                        lo[axis] = lo[axis].min(-along(-1.0)?.dist);
                        hi[axis] = hi[axis].max(along(1.0)?.dist);
                    }
                }
                continue;
            };
            stack.extend(self.nodes[index].children);
        }
        Some((lo, hi))
    }
}

/// The nearest hit of a sweep through several models, in the world.
#[derive(Debug, Clone, Copy)]
struct Found {
    fraction: f32,
    normal: Vec3,
    start_solid: bool,
    all_solid: bool,
    left_solid: f32,
    contents: u32,
    surface: Surface,
    displacement: bool,
    model: Option<usize>,
    static_prop: Option<usize>,
}

impl Default for Found {
    fn default() -> Found {
        Found {
            fraction: 1.0,
            normal: [0.0; 3],
            start_solid: false,
            all_solid: false,
            left_solid: 0.0,
            contents: contents::EMPTY,
            surface: Surface::default(),
            displacement: false,
            model: None,
            static_prop: None,
        }
    }
}

impl Found {
    /// What a sweep through one model found, from the space it stands in: kept when nearer.
    fn take(&mut self, sweep: &Sweep, frame: &Frame, model: Option<usize>, static_prop: Option<usize>) {
        self.start_solid |= sweep.start_solid;
        self.all_solid |= sweep.all_solid;
        self.left_solid = self.left_solid.max(sweep.left_solid);
        if let Some(hit) = sweep.hit {
            self.fraction = sweep.fraction;
            self.normal = frame.world_direction(hit.normal);
            self.contents = hit.contents;
            self.surface = hit.surface;
            self.displacement = hit.displacement;
            (self.model, self.static_prop) = (model, static_prop);
        }
    }

    fn trace(self, start: Vec3, end: Vec3) -> Trace {
        Trace {
            fraction: self.fraction,
            end: if self.fraction >= 1.0 { end } else { lerp3(start, end, self.fraction) },
            normal: self.normal,
            start_solid: self.start_solid,
            all_solid: self.all_solid,
            fraction_left_solid: self.left_solid,
            contents: self.contents,
            texinfo: self.surface.texinfo.map(|t| t as usize),
            surface_flags: self.surface.flags,
            displacement: self.displacement,
            model: self.model,
            static_prop: self.static_prop,
        }
    }
}

/// The displacements of the world, as triangles facing the way the drawn ones do.
fn displacements(bsp: &Bsp, surface: &impl Fn(i16) -> Surface) -> Vec<Displacement> {
    let faces = if bsp.faces.is_empty() { &bsp.faces_hdr } else { &bsp.faces };
    let Some(world) = bsp.models.first() else { return Vec::new() };
    let mut out = Vec::new();
    for face in faces.iter().skip(world.first_face.max(0) as usize).take(world.num_faces.max(0) as usize) {
        if face.dispinfo < 0 {
            continue;
        }
        let Some(points) = mesh::displacement_points(bsp, face) else { continue };
        let disp = &bsp.displacements[face.dispinfo as usize];
        let mut triangles = Vec::new();
        let mut sum = [0.0f32; 3];
        for [a, b, c] in mesh::displacement_triangles(disp.side()) {
            let points = [points[a], points[b], points[c]];
            let n = cross(sub(points[1], points[0]), sub(points[2], points[0]));
            sum = add(sum, n);
            if length(n) < 1e-6 {
                continue;
            }
            let mins = std::array::from_fn(|i| points[0][i].min(points[1][i]).min(points[2][i]));
            let maxs = std::array::from_fn(|i| points[0][i].max(points[1][i]).max(points[2][i]));
            triangles.push(Triangle { points, normal: normalize(n), mins, maxs });
        }
        // the winding says which way is the front only together with the base face, as when
        // the displacement is drawn
        if dot(sum, bsp.planes[face.plane as usize].normal) < 0.0 {
            for tri in &mut triangles {
                tri.normal = scale(tri.normal, -1.0);
            }
        }
        let Some(first) = triangles.first() else { continue };
        let (mut mins, mut maxs) = (first.mins, first.maxs);
        for tri in &triangles {
            mins = std::array::from_fn(|i| mins[i].min(tri.mins[i]));
            maxs = std::array::from_fn(|i| maxs[i].max(tri.maxs[i]));
        }
        // a displacement takes the contents of its brush; none at all is solid ground still
        let contents = match disp.contents as u32 {
            contents::EMPTY => contents::SOLID,
            c => c,
        };
        out.push(Displacement { contents, surface: surface(face.texinfo), mins, maxs, triangles });
    }
    out
}

/// The world, and the brush entities solid when the map starts, where the map puts them -
/// or, not `solid`, the brush entities that may move and are not solid at the start.
fn placements(bsp: &Bsp, solid: bool) -> Vec<Placement> {
    let mut out = if solid { vec![Placement::new(0, None, [0.0; 3], [0.0; 3])] } else { Vec::new() };
    for (i, entity) in bsp.entities.iter().enumerate() {
        let Some(model) = entity.brush_model() else { continue };
        let moves = entity.moves();
        if model >= bsp.models.len() || solid_at_start(entity) != solid || !solid && (!moves || never_solid(entity)) {
            continue;
        }
        let mut placement =
            Placement::new(model, Some(i), entity.vec3("origin").unwrap_or([0.0; 3]), entity.vec3("angles").unwrap_or([0.0; 3]));
        placement.moves = moves;
        out.push(placement);
    }
    out
}

#[cfg(test)]
mod tests;
