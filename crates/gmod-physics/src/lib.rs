//! Physics of props as Garry's Mod has them: bodies made of convex hulls - the solids of a
//! model's `.phy` - fall, tumble, hit each other and come to rest in a world of brushes,
//! displacements and static props, and fall asleep when they stop. On Rapier.
//!
//! Everything here counts as the game does: units (inches) for positions and velocities,
//! degrees for angles (pitch, yaw, roll, turned as the engine turns models) and for turning
//! speeds, kilograms for masses. Rapier counts in metres and radians inside. Gravity is
//! `sv_gravity`, [`DEFAULT_GRAVITY`] units/s² down unless told otherwise.
//!
//! A body is frozen (`EnableMotion(false)`) by making it fixed: it stays where it is and others
//! still hit it. One without collisions (`EnableCollisions(false)`) falls through everything.
//!
//! Bodies are held together by [`Joint`]s - welds, ropes, hinges, what the constraints of GMod
//! are made of - to each other or to the world ([`Physics::ground`]); a joint given a strength
//! breaks when pulled harder than that. The parts of a ragdoll hang together by
//! [`Joint::Ragdoll`]s, each turning only as far as its limits about each axis. Two bodies can
//! be set to go through each other ([`Physics::set_apart`]), and one through every body that
//! moves ([`Physics::set_aloof`]), as a corpse that only lies on the world.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use rapier3d::math::{Mat3, Pose, Rotation, Vector};
use rapier3d::prelude::{
    ActiveHooks, CoefficientCombineRule, Collider, ColliderBuilder, ColliderHandle, ContactModificationContext, GenericJoint,
    GenericJointBuilder, ImpulseJointHandle, InteractionGroups, JointAxesMask, JointAxis, MassProperties, ModifiableContacts,
    PairFilterContext, PhysicsHooks, PhysicsWorld, QueryFilter, Ray, RigidBodyBuilder, RigidBodyHandle, RigidBodyType, SharedShape,
    SolverFlags, SpringJointBuilder,
};

pub type Vec3 = [f32; 3];

/// A unit in metres.
pub const METRES_PER_UNIT: f32 = 0.0254;
/// `sv_gravity`: units/s² down.
pub const DEFAULT_GRAVITY: f32 = 600.0;
/// Kilograms in a cubic metre of what has no mass of its own: the `default` surface property.
pub const DEFAULT_DENSITY: f32 = 2000.0;
/// How much what slides along something holds back, when nothing says otherwise. Two things
/// sliding along each other hold back as much as the two together, multiplied, as IVP has it
/// (measured in GMod: a crate of wood, 0.8, on concrete, 0.8, slides as 0.64; of glass, 0.5,
/// as 0.4): a body of `friction_00` slides on anything.
pub const DEFAULT_FRICTION: f32 = 0.8;

/// How much of its speed into a surface something keeps bouncing off it, of the `default`
/// surface, and of the world's: two together bounce by the square root of the product of
/// theirs, no less than [`LEAST_BOUNCE`] and no more than all of it (as GMod, measured).
pub const DEFAULT_ELASTICITY: f32 = 0.25;

/// The least two bodies bounce off each other, whatever they are made of: in GMod a ball of
/// dirt (0.01) bounces off grass (0.01) as one of concrete (0.2) off concrete, by 0.2 of its
/// speed.
pub const LEAST_BOUNCE: f32 = 0.2;

/// How much two surfaces bounce off each other, of the elasticity of each.
fn bounce(elasticity1: f32, elasticity2: f32) -> f32 {
    (elasticity1.max(0.0) * elasticity2.max(0.0)).sqrt().clamp(LEAST_BOUNCE, 1.0)
}

/// Which of the points where two faces touch are corners of where they touch: those inside
/// the others (seen along `normal`) hold nothing the corners do not, and pushing on one of
/// them, off the middle, would set what lands flat turning.
fn corners(points: &[Vector], normal: Vector) -> Vec<bool> {
    let side = if normal.x.abs() < 0.9 { Vector::X } else { Vector::Y };
    let u = normal.cross(side).normalize();
    let v = normal.cross(u);
    let flat: Vec<[f32; 2]> = points.iter().map(|p| [p.dot(u), p.dot(v)]).collect();
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    // well inside a triangle of three others
    let margin = 0.5 * METRES_PER_UNIT;
    let inside = |p: [f32; 2], [a, b, c]: [[f32; 2]; 3]| {
        let area = cross(a, b, c);
        if area.abs() < 1e-6 {
            return false;
        }
        let sign = area.signum();
        [(a, b), (b, c), (c, a)].iter().all(|&(x, y)| {
            let edge = ((y[0] - x[0]).powi(2) + (y[1] - x[1]).powi(2)).sqrt();
            sign * cross(x, y, p) > margin * edge
        })
    };
    (0..flat.len())
        .map(|i| {
            let others: Vec<[f32; 2]> = (0..flat.len()).filter(|&j| j != i).map(|j| flat[j]).collect();
            let n = others.len();
            !(0..n).any(|a| (a + 1..n).any(|b| (b + 1..n).any(|c| inside(flat[i], [others[a], others[b], others[c]]))))
        })
        .collect()
}

/// Slower than this into each other, units a second, two bodies do not bounce: they rest.
const LEAST_BOUNCING_SPEED: f32 = 10.0;

/// What a contact of two colliders is made of: the friction of a solid of the world of many
/// surfaces with the other (none for others, whose own combine), and how much the two bounce.
/// A solid of many surfaces touches by the one touched: the face most facing what touches it
/// (`normal` goes from the first collider to the second), the triangle touched.
fn contact_material(
    made_of: &HashMap<ColliderHandle, MadeOf>,
    colliders: &rapier3d::prelude::ColliderSet,
    pair: [ColliderHandle; 2],
    normal: Vector,
    subshapes: [u32; 2],
) -> (Option<f32>, f32) {
    let (Some(a), Some(b)) = (colliders.get(pair[0]), colliders.get(pair[1])) else { return (None, LEAST_BOUNCE) };
    let made = [(pair[0], b, normal, subshapes[0]), (pair[1], a, -normal, subshapes[1])]
        .into_iter()
        .find_map(|(solid, other, out, subshape)| made_of.get(&solid).map(|m| (m, other, out, subshape)));
    if let Some((made, other, out, subshape)) = made {
        let surface = match made {
            MadeOf::Faces(faces) => faces.iter().max_by(|a, b| a.0.dot(out).total_cmp(&b.0.dot(out))).map(|&(_, s)| s),
            MadeOf::Triangles(surfaces) => surfaces.get(subshape as usize).copied(),
        };
        if let Some(surface) = surface {
            return (Some(surface.friction.max(0.0) * other.friction()), bounce(surface.elasticity, other.restitution()));
        }
    }
    (None, bounce(a.restitution(), b.restitution()))
}

/// What a surface is made of, as far as what touches it goes: how much it holds back what
/// slides along it, and how bouncy it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub friction: f32,
    pub elasticity: f32,
}

impl Default for Surface {
    fn default() -> Surface {
        Surface { friction: DEFAULT_FRICTION, elasticity: DEFAULT_ELASTICITY }
    }
}

/// What the parts of a solid of the world are made of, when they are not all one surface: a
/// brush's faces by the normal facing out of each, a surface of triangles by triangle.
enum MadeOf {
    Faces(Vec<(Vector, Surface)>),
    Triangles(Vec<Surface>),
}

/// A collider of the `default` surface, its friction and bounce with another's as GMod's: the
/// product of the two frictions, the square root of the product of the two elasticities.
/// Its contacts are looked at, for their bounce to be no less nor more than it can be
/// ([`bounce`]).
fn of_default_surface(collider: ColliderBuilder) -> ColliderBuilder {
    collider
        .friction(DEFAULT_FRICTION)
        .friction_combine_rule(CoefficientCombineRule::Multiply)
        .restitution(DEFAULT_ELASTICITY)
        .restitution_combine_rule(CoefficientCombineRule::GeometricMean)
        .active_hooks(ActiveHooks::MODIFY_SOLVER_CONTACTS)
}

/// How hard a rigid rope holds its length against being pushed shorter: a spring of this
/// stiffness and damping per kilogram (1/s², 1/s), critically damped and far quicker than a
/// tick.
const ROD_STIFFNESS: f32 = 1.0e5;
/// A ragdoll joint's limit this close to none, in radians (0.06 degrees), is none.
const STILL_LIMIT: f32 = 1.0e-3;
const ROD_DAMPING: f32 = 632.0;
/// How far apart two bodies are to have been, units, for their touching again to be an
/// [`Impact`], however slow: in GMod a crate lifted 0.55 off the floor and sent down at 120
/// units/s makes none, one lifted 0.65 and sent down at 5 makes one.
const IMPACT_SEPARATION: f32 = 0.6;

fn metres(v: Vec3) -> Vector {
    Vector::new(v[0], v[1], v[2]) * METRES_PER_UNIT
}

fn units(v: Vector) -> Vec3 {
    (v / METRES_PER_UNIT).to_array()
}

/// The axes - forward, left, up - of something turned by `angles` (pitch, yaw, roll in degrees).
pub fn axes(angles: Vec3) -> [Vec3; 3] {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let ((sp, cp), (sy, cy), (sr, cr)) = (pitch.sin_cos(), yaw.sin_cos(), roll.sin_cos());
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

/// The turn of `angles`.
pub fn rotation(angles: Vec3) -> Rotation {
    let [f, l, u] = axes(angles);
    Rotation::from_mat3(&Mat3::from_cols(f.into(), l.into(), u.into())).normalize()
}

/// The angles of a turn, as the engine reads them from its axes: yaw from the forward axis on
/// the ground, pitch from how far it points down, roll from the left axis.
pub fn angles(rotation: Rotation) -> Vec3 {
    let m = Mat3::from_quat(rotation);
    let (forward, left, up) = (m.x_axis, m.y_axis, m.z_axis);
    let flat = (forward.x * forward.x + forward.y * forward.y).sqrt();
    let (pitch, yaw, roll) = if flat > 0.001 {
        ((-forward.z).atan2(flat), forward.y.atan2(forward.x), left.z.atan2(up.z))
    } else {
        // straight up or down: the yaw goes into the left axis, no roll
        ((-forward.z).atan2(flat), (-left.x).atan2(left.y), 0.0)
    };
    [pitch, yaw, roll].map(f32::to_degrees)
}

/// What a body is made of, in its own space.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Convex solids by their corners: the solids of a `.phy`, or a box.
    Hulls(Vec<Vec<Vec3>>),
    /// A ball around the body's origin, of this radius.
    Sphere(f32),
}

impl Shape {
    /// A box from `mins` to `maxs`.
    pub fn cuboid(mins: Vec3, maxs: Vec3) -> Shape {
        let corners = (0..8).map(|i| std::array::from_fn(|axis| if i & (1 << axis) == 0 { mins[axis] } else { maxs[axis] })).collect();
        Shape::Hulls(vec![corners])
    }

    /// The shapes for Rapier, in metres, with their volumes; none for what has no volume.
    fn shared(&self) -> Vec<(SharedShape, f32)> {
        let shapes = match self {
            Shape::Hulls(hulls) => hulls
                .iter()
                .filter_map(|corners| SharedShape::convex_hull(&corners.iter().map(|&p| metres(p)).collect::<Vec<_>>()))
                .collect(),
            Shape::Sphere(radius) => vec![SharedShape::ball(radius * METRES_PER_UNIT)],
        };
        // a cubic millimetre is nothing
        shapes
            .into_iter()
            .map(|s| {
                let volume = s.mass_properties(1.0).mass();
                (s, volume)
            })
            .filter(|&(_, v)| v > 1e-9)
            .collect()
    }
}

/// A body to add.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub shape: Shape,
    /// Kilograms; none for as heavy as [`DEFAULT_DENSITY`] makes it.
    pub mass: Option<f32>,
    pub origin: Vec3,
    pub angles: Vec3,
    /// How fast moving and turning die away by themselves, per second (a `.phy`'s `damping`
    /// and `rotdamping`), while it touches something: as IVP's, damping holds back nothing in
    /// the air ([`Physics::set_damping`]).
    pub damping: f32,
    pub rot_damping: f32,
    /// How much harder it is to turn than its shape makes it: a `.phy`'s `inertia`, which
    /// ragdolls set to keep their light parts from spinning on their joints.
    pub inertia: f32,
    /// Whatever the caller keeps with it: its entity.
    pub user: u64,
}

impl Body {
    pub fn new(shape: Shape, origin: Vec3, angles: Vec3) -> Body {
        Body { shape, mass: None, origin, angles, damping: 0.0, rot_damping: 0.0, inertia: 1.0, user: 0 }
    }
}

/// A body in a [`Physics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BodyId(RigidBodyHandle);

/// What a ray hit ([`Physics::ray`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub body: BodyId,
    /// How far along: 0 at the start, 1 at the end.
    pub fraction: f32,
    /// Facing back at the ray.
    pub normal: Vec3,
}

/// How two bodies are held together. Points and axes are in the world, as the bodies are when
/// they are joined.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Joint {
    /// As they are to each other: a weld.
    Weld,
    /// Turning any way about a point: a ball and socket.
    Ball { point: Vec3 },
    /// Turning about the line through `point` along `axis`: a hinge.
    Hinge { point: Vec3, axis: Vec3 },
    /// Sliding along the line through `point` along `axis`, not turning.
    Slider { point: Vec3, axis: Vec3 },
    /// A point of each, `start` of the first and `end` of the second, no farther apart than
    /// `length` units; no nearer either when `rigid`.
    Rope { start: Vec3, end: Vec3, length: f32, rigid: bool },
    /// A point of each pulled towards `length` units apart, with `stiffness` (kilograms per
    /// second squared) and `damping` (kilograms per second) along the line between them; and
    /// `relative_damping` (kilograms per second) against their moving apart any way at all,
    /// as VPhysics' springs have it (measured in GMod). Only stretching, it does nothing while
    /// they are nearer than `length`: it pulls, and never pushes.
    Spring { start: Vec3, end: Vec3, length: f32, stiffness: f32, damping: f32, relative_damping: f32, stretch_only: bool },
    /// Nothing held: the two only go through each other.
    NoCollide,
    /// Turning about `point`, the second no farther from how it is now to the first than
    /// `limits`: for each axis (x, y, z) of a frame turned by `angles`, the least and the most
    /// degrees, -180 to 180. An axis whose limits are both 0 does not turn at all. A `free` one
    /// holds only how the second turns, not where it goes. A joint of a ragdoll (a `.phy`'s
    /// `ragdollconstraint`), and `phys_ragdollconstraint`.
    Ragdoll { point: Vec3, angles: Vec3, limits: [[f32; 2]; 3], free: bool },
}

/// A joint in a [`Physics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JointId(ImpulseJointHandle);

/// The world and the bodies in it.
pub struct Physics {
    world: PhysicsWorld,
    /// What never moves and has no shape: the world, for joints.
    ground: RigidBodyHandle,
    /// The joints that break, and the force and torque they take: newtons, newton metres.
    strengths: HashMap<JointId, [f32; 2]>,
    /// Joints broken since they were last asked for.
    broken: Vec<JointId>,
    /// Bodies that go through each other, each pair both ways round ([`Physics::set_apart`]).
    apart: HashSet<(RigidBodyHandle, RigidBodyHandle)>,
    /// Bodies that go through every other body that moves ([`Physics::set_aloof`]).
    aloof: HashSet<RigidBodyHandle>,
    /// The collision groups of the bodies that have one but none, and what says which two
    /// collide ([`Physics::set_collision_group`]).
    groups: HashMap<RigidBodyHandle, i64>,
    group_rule: Option<fn(i64, i64) -> bool>,
    /// Impacts since they were last asked for, and the colliders that have touched since they
    /// were last apart ([`IMPACT_SEPARATION`]).
    impacts: Vec<Impact>,
    together: HashSet<(ColliderHandle, ColliderHandle)>,
    /// When each two bodies (none: the world) last hit each other, for a second.
    last_impacts: HashMap<(Option<RigidBodyHandle>, Option<RigidBodyHandle>), f32>,
    /// The ragdoll joints and their limits.
    limits: Vec<Limit>,
    /// The springs that hold back their two points moving apart any way, and how much:
    /// kilograms per second ([`Joint::Spring`]'s `relative_damping`).
    dampers: HashMap<JointId, f32>,
    /// How fast the moving and turning of each body that has damping die away, per second,
    /// while it touches something ([`Physics::set_damping`]).
    damping: HashMap<RigidBodyHandle, (f32, f32)>,
    /// The springs that only stretch, and how: their length (metres), stiffness and damping.
    slack: HashMap<JointId, [f32; 3]>,
    /// The bodies harder to turn than their shapes ([`Body::inertia`]), and how much.
    inertia: HashMap<RigidBodyHandle, f32>,
    /// Seconds stepped.
    clock: f32,
    /// The most a body moves and turns: units and degrees a second (`physenv`'s performance
    /// settings, `MaxVelocity` and `MaxAngularVelocity`).
    speed_limits: [f32; 2],
    /// The solids of the world of more than one surface ([`Physics::add_world_made_of`]).
    made_of: HashMap<ColliderHandle, MadeOf>,
    /// The solids of the world that touch others, and those they touch ([`Physics::add_world`]).
    seams: HashMap<ColliderHandle, Vec<ColliderHandle>>,
    /// The solids of the world, convex.
    solids: Vec<ColliderHandle>,
}

/// How far out of a solid of the world a contact is looked past, metres: into a neighbour, it
/// is a face between the two, which nothing touches.
const SEAM_PROBE: f32 = 0.01;

/// GMod's limits of how fast a body moves and turns, units and degrees a second, as
/// `physenv.GetPerformanceSettings` gives them (measured).
pub const MAX_VELOCITY: f32 = 4000.0;
pub const MAX_ANGULAR_VELOCITY: f32 = 7272.728;

/// Where a body is to go, and how, for [`Physics::shadow`]: as Source's shadow controllers
/// steer what they hold (`ComputeShadowControl`), the physics gun's among them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub origin: Vec3,
    pub angles: Vec3,
    /// Seconds to get there.
    pub seconds: f32,
    /// The most it goes, units a second, and the most of its own speed held back in a step.
    pub max_speed: f32,
    pub max_damp_speed: f32,
    /// The same for turning, degrees a second.
    pub max_angular: f32,
    pub max_damp_angular: f32,
    /// How much of its own speed is held back in a step, 0 to 1.
    pub damp_factor: f32,
    /// Farther than this from where it is to be, it is put there; 0 for never.
    pub teleport_distance: f32,
}

/// One part of a shadow's control, as IVP's `ComputeController`: the speed `v` held back by
/// `damp` of itself (no more than `max_damp` a step), then pushed towards `delta` at `scale`
/// of it a second (no more than `max_speed`).
fn shadow_control(v: Vector, delta: Vector, max_speed: f32, max_damp: f32, scale: f32, damp: f32) -> Vector {
    let mut v = v;
    if v.length_squared() < 1e-6 {
        v = Vector::ZERO;
    } else if max_damp > 0.0 {
        let held = v.length() * damp.abs();
        let back = -v * damp;
        v += if held > max_damp { back * (max_damp / held) } else { back };
    }
    if max_speed > 0.0 {
        let push = delta * scale;
        let fast = push.length();
        v += if fast > max_speed { push * (max_speed / fast) } else { push };
    }
    v
}

/// A body hit something hard enough to be heard, as Source tells its entities
/// (`PhysicsCollide`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Impact {
    pub body: BodyId,
    /// The body's caller value (its entity).
    pub user: u64,
    /// What it hit: another body; `None` for the world.
    pub other_body: Option<BodyId>,
    /// And its caller value.
    pub other: Option<u64>,
    /// How fast it went into what it hit, along the normal, in units/s.
    pub speed: f32,
    /// Where, in the world.
    pub point: Vec3,
    /// From the body towards what it hit.
    pub normal: Vec3,
    /// How the two were going before it, units/s: the body, and what it hit.
    pub velocity: Vec3,
    pub other_velocity: Vec3,
    /// Seconds since the two last hit each other, no more than 1.
    pub since: f32,
}

/// A body touching something now (`GetFrictionSnapshot`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    /// What it touches: another body; `None` for the world.
    pub other: Option<BodyId>,
    /// Where, in the world.
    pub point: Vec3,
    /// From the body towards what it touches.
    pub normal: Vec3,
    /// How hard they press on each other in the last step, in the engine's units of force
    /// (kg units/s²).
    pub force: f32,
}

impl Default for Physics {
    fn default() -> Physics {
        Physics::new()
    }
}

impl Physics {
    /// An empty world with the default gravity.
    pub fn new() -> Physics {
        let mut world = PhysicsWorld::new();
        world.gravity = metres([0.0, 0.0, -DEFAULT_GRAVITY]);
        // every point of a contact kept: Rapier's clustering of them takes one side and then
        // the other of what rests across a crest (a block on a displacement's ridge), and it
        // rocks on it for ever
        world.integration_parameters.contact_clustering = false;
        // the joints worked out four times a solver iteration: what is held by stiff joints
        // against each other (a simfphys car's wheels: rods, springs, a ball joint) is left
        // with forces that do not cancel otherwise, and creeps over the ground on its own
        world.integration_parameters.num_internal_pgs_iterations = 4;
        let ground = world.insert_body(RigidBodyBuilder::fixed());
        Physics {
            world,
            ground,
            strengths: HashMap::new(),
            dampers: HashMap::new(),
            damping: HashMap::new(),
            slack: HashMap::new(),
            broken: Vec::new(),
            apart: HashSet::new(),
            aloof: HashSet::new(),
            groups: HashMap::new(),
            group_rule: None,
            impacts: Vec::new(),
            together: HashSet::new(),
            last_impacts: HashMap::new(),
            limits: Vec::new(),
            inertia: HashMap::new(),
            clock: 0.0,
            speed_limits: [MAX_VELOCITY, MAX_ANGULAR_VELOCITY],
            seams: HashMap::new(),
            made_of: HashMap::new(),
            solids: Vec::new(),
        }
    }

    /// Finds which solids of the world touch which, so that what slides over the seam of two
    /// does not meet the face between them ([`Apart::modify_solver_contacts`]): side by side,
    /// their boxes overlap, swept along x.
    fn join_seams(&mut self) {
        let margin = 0.1 * METRES_PER_UNIT;
        self.seams.clear();
        let mut boxes: Vec<(ColliderHandle, Vector, Vector)> = self
            .solids
            .iter()
            .filter_map(|&h| self.world.colliders.get(h).map(|c| (h, c.compute_aabb())))
            .map(|(h, aabb)| (h, aabb.mins - Vector::splat(margin), aabb.maxs + Vector::splat(margin)))
            .collect();
        boxes.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
        for i in 0..boxes.len() {
            let (a, lo, hi) = boxes[i];
            for &(b, lo2, hi2) in &boxes[i + 1..] {
                if lo2.x > hi.x {
                    break;
                }
                if lo.y <= hi2.y && lo2.y <= hi.y && lo.z <= hi2.z && lo2.z <= hi.z {
                    self.seams.entry(a).or_default().push(b);
                    self.seams.entry(b).or_default().push(a);
                }
            }
        }
        for &h in &self.solids {
            if self.seams.contains_key(&h)
                && let Some(collider) = self.world.colliders.get_mut(h)
            {
                collider.set_active_hooks(collider.active_hooks() | ActiveHooks::MODIFY_SOLVER_CONTACTS);
            }
        }
    }

    /// The most a body moves and turns from now on, units and degrees a second.
    pub fn set_speed_limits(&mut self, speed: f32, turn: f32) {
        self.speed_limits = [speed.max(0.0), turn.max(0.0)];
    }

    /// Gravity from now on as a direction too: units/s² (`physenv.SetGravity`).
    pub fn set_gravity_vector(&mut self, gravity: Vec3) {
        self.world.gravity = metres(gravity);
    }

    /// Gravity, units/s².
    pub fn gravity(&self) -> Vec3 {
        let g = self.world.gravity / METRES_PER_UNIT;
        [g.x, g.y, g.z]
    }

    /// Steers a body as Source's shadow controllers steer what they hold
    /// (`ComputeShadowControl`), a step of `dt` seconds: what it was going keeps on, held back
    /// by `damp_factor` of it, and it is pushed towards the pose to be there in `seconds`; its
    /// origin goes and it turns about it. What is in the way still stops it.
    pub fn shadow(&mut self, id: BodyId, shadow: &Shadow, dt: f32) {
        let Some(body) = self.world.bodies.get_mut(id.0) else { return };
        let dt = dt.max(1e-4);
        let fraction = if shadow.seconds > 0.0 { (dt / shadow.seconds).min(1.0) } else { 1.0 };
        let scale = fraction / dt;
        let pose = body.translation();
        let centre = body.center_of_mass();
        let target = metres(shadow.origin);
        let mut delta = target - pose;
        let turn_to = rotation(shadow.angles);
        if shadow.teleport_distance > 0.0 && delta.length() > shadow.teleport_distance * METRES_PER_UNIT {
            body.set_position(Pose::from_parts(target, turn_to), true);
            delta = Vector::ZERO;
        }
        // the shortest way round from how it is turned to how it should be
        let (axis, mut angle) = (turn_to * body.rotation().inverse()).to_axis_angle();
        if angle > std::f32::consts::PI {
            angle -= std::f32::consts::TAU;
        }
        let pose = body.translation();
        let w = body.angvel();
        // the speed of its origin, which is steered
        let v = body.linvel() + w.cross(pose - centre);
        let v = shadow_control(
            v,
            delta,
            shadow.max_speed * METRES_PER_UNIT,
            shadow.max_damp_speed * METRES_PER_UNIT,
            scale,
            shadow.damp_factor,
        );
        let w = shadow_control(
            w,
            axis * angle,
            shadow.max_angular.to_radians(),
            shadow.max_damp_angular.to_radians(),
            scale,
            shadow.damp_factor,
        );
        body.set_linvel(v + w.cross(centre - pose), true);
        body.set_angvel(w, true);
    }

    /// The world as a body, to join bodies to: it never moves.
    pub fn ground(&self) -> BodyId {
        BodyId(self.ground)
    }

    /// Gravity from now on: `sv_gravity`, units/s² down.
    pub fn set_gravity(&mut self, gravity: f32) {
        self.world.gravity = metres([0.0, 0.0, -gravity]);
    }

    /// Adds what never moves, in the world's space: convex solids by their corners, and
    /// triangles. Returns how many solids could be made of `hulls`; the rest had no volume.
    pub fn add_world(&mut self, hulls: &[Vec<Vec3>], vertices: &[Vec3], triangles: &[[u32; 3]]) -> usize {
        self.add_world_made_of(hulls, &[], vertices, triangles, &[])
    }

    /// The same as [`Physics::add_world`], of surfaces: each face of each hull, by the normal
    /// facing out of it, and each triangle; of the `default` surface where none is given. What
    /// touches a solid is touched by the face it touches, as VPhysics' world is made.
    pub fn add_world_made_of(
        &mut self,
        hulls: &[Vec<Vec3>],
        faces: &[Vec<(Vec3, Surface)>],
        vertices: &[Vec3],
        triangles: &[[u32; 3]],
        triangle_surfaces: &[Surface],
    ) -> usize {
        let mut added = Vec::new();
        for (i, corners) in hulls.iter().enumerate() {
            let points: Vec<Vector> = corners.iter().map(|&p| metres(p)).collect();
            if let Some(collider) = ColliderBuilder::convex_hull(&points) {
                let handle = self.world.colliders.insert(of_default_surface(collider));
                let faces = faces.get(i).map(|f| f.iter().map(|&(normal, surface)| (Vector::from(normal), surface)).collect());
                self.made_of_parts(handle, MadeOf::Faces(faces.unwrap_or_default()));
                added.push(handle);
            }
        }
        self.solids.extend(&added);
        self.join_seams();
        let added = added.len();
        // the triangles as one surface, their shared edges known: what slides or lies over
        // an edge between two of them is not knocked by it (a contact's normal there would
        // be the edge's), from either side
        let flags = rapier3d::prelude::TriMeshFlags::FIX_INTERNAL_EDGES_TWO_SIDED;
        if !triangles.is_empty()
            && let Ok(collider) =
                ColliderBuilder::trimesh_with_flags(vertices.iter().map(|&p| metres(p)).collect(), triangles.to_vec(), flags)
        {
            let handle = self.world.colliders.insert(of_default_surface(collider));
            self.made_of_parts(handle, MadeOf::Triangles(triangle_surfaces.to_vec()));
        }
        added
    }

    /// A solid of the world is made of `parts`: of one surface, it is all of it; of more, each
    /// contact with it is of the part touched ([`Apart::modify_solver_contacts`]).
    fn made_of_parts(&mut self, handle: ColliderHandle, parts: MadeOf) {
        let surfaces: Vec<Surface> = match &parts {
            MadeOf::Faces(faces) => faces.iter().map(|&(_, s)| s).collect(),
            MadeOf::Triangles(surfaces) => surfaces.clone(),
        };
        let Some(collider) = self.world.colliders.get_mut(handle) else { return };
        let Some(&first) = surfaces.first() else { return };
        collider.set_friction(first.friction.max(0.0));
        collider.set_restitution(first.elasticity.max(0.0));
        if surfaces.iter().any(|&s| s != first) {
            collider.set_active_hooks(collider.active_hooks() | ActiveHooks::MODIFY_SOLVER_CONTACTS);
            self.made_of.insert(handle, parts);
        }
    }

    /// Adds a body, awake and moving; none when its shape has no volume.
    pub fn add(&mut self, body: &Body) -> Option<BodyId> {
        let shapes = body.shape.shared();
        if shapes.is_empty() {
            return None;
        }
        // the mass shared by volume
        let volume: f32 = shapes.iter().map(|&(_, v)| v).sum();
        let density = match body.mass {
            Some(mass) if mass > 0.0 && volume > 0.0 => mass / volume,
            _ => DEFAULT_DENSITY,
        };
        let rigid =
            RigidBodyBuilder::dynamic().pose(Pose::from_parts(metres(body.origin), rotation(body.angles))).user_data(u128::from(body.user));
        let handle = self.world.insert_body(rigid);
        for (shape, _) in shapes {
            let collider = of_default_surface(ColliderBuilder::new(shape).density(density));
            self.world.insert_collider(collider, Some(handle));
        }
        if body.inertia > 0.0 && body.inertia != 1.0 {
            self.inertia.insert(handle, body.inertia);
        }
        self.set_damping(BodyId(handle), body.damping, body.rot_damping);
        self.weigh(handle);
        Some(BodyId(handle))
    }

    /// How much a body holds back what slides along it: its material's friction.
    pub fn set_friction(&mut self, id: BodyId, friction: f32) {
        let Some(body) = self.world.bodies.get(id.0) else { return };
        for &collider in body.colliders() {
            if let Some(collider) = self.world.colliders.get_mut(collider) {
                collider.set_friction(friction.max(0.0));
            }
        }
    }

    /// How bouncy a body is: its material's elasticity ([`DEFAULT_ELASTICITY`]).
    pub fn set_elasticity(&mut self, id: BodyId, elasticity: f32) {
        let Some(body) = self.world.bodies.get(id.0) else { return };
        for &collider in body.colliders() {
            if let Some(collider) = self.world.colliders.get_mut(collider) {
                collider.set_restitution(elasticity.max(0.0));
            }
        }
    }

    /// The friction of a body ([`Physics::set_friction`]).
    pub fn friction(&self, id: BodyId) -> f32 {
        let body = self.world.bodies.get(id.0);
        body.and_then(|b| b.colliders().first()).and_then(|&c| self.world.colliders.get(c)).map_or(DEFAULT_FRICTION, |c| c.friction())
    }

    /// Works out a body's mass and inertia from its colliders, and turns it as hard as it
    /// should ([`Body::inertia`]): its inertia, scaled, about where its mass is.
    fn weigh(&mut self, handle: RigidBodyHandle) {
        let Some(body) = self.world.bodies.get_mut(handle) else { return };
        body.set_additional_mass_properties(MassProperties::default(), false);
        body.recompute_mass_properties_from_colliders(&self.world.colliders);
        let Some(&scale) = self.inertia.get(&handle) else { return };
        let own = body.mass_properties().local_mprops;
        let more = own.inv_principal_inertia.map(|i| if i > 0.0 { (scale - 1.0) / i } else { 0.0 });
        let extra = MassProperties::with_principal_inertia_frame(own.local_com, 0.0, more, own.principal_inertia_local_frame);
        body.set_additional_mass_properties(extra, false);
        body.recompute_mass_properties_from_colliders(&self.world.colliders);
    }

    /// Adds a brush that moves as the game moves it (a door, a platform): convex solids by
    /// their corners in its own space, where `origin` and `angles` put it. It is not pushed by
    /// anything, and pushes what is in its way when it moves ([`Physics::move_mover`]). None
    /// when no solid of it has volume.
    pub fn add_mover(&mut self, hulls: &[Vec<Vec3>], origin: Vec3, angles: Vec3, user: u64) -> Option<BodyId> {
        let colliders: Vec<_> = hulls
            .iter()
            .filter_map(|corners| ColliderBuilder::convex_hull(&corners.iter().map(|&p| metres(p)).collect::<Vec<_>>()))
            .collect();
        if colliders.is_empty() {
            return None;
        }
        let rigid = RigidBodyBuilder::kinematic_position_based()
            .pose(Pose::from_parts(metres(origin), rotation(angles)))
            .user_data(u128::from(user));
        let handle = self.world.insert_body(rigid);
        for collider in colliders {
            self.world.insert_collider(of_default_surface(collider), Some(handle));
        }
        Some(BodyId(handle))
    }

    /// Where a mover is to be at the end of the next step: it goes there during the step, at
    /// the speed that takes, and carries or pushes what it meets. What sleeps against it wakes.
    pub fn move_mover(&mut self, id: BodyId, origin: Vec3, angles: Vec3) {
        let Some(body) = self.world.bodies.get_mut(id.0) else { return };
        if !body.is_kinematic() {
            return;
        }
        let to = Pose::from_parts(metres(origin), rotation(angles));
        if *body.position() == to {
            return;
        }
        body.set_next_kinematic_position(to);
        body.wake_up(true);
    }

    /// Takes a body away, and its joints. The ground stays.
    pub fn remove(&mut self, id: BodyId) {
        if id.0 != self.ground {
            self.world.remove_body(id.0);
            self.apart.retain(|&(a, b)| a != id.0 && b != id.0);
            self.aloof.remove(&id.0);
            self.inertia.remove(&id.0);
        }
    }

    pub fn contains(&self, id: BodyId) -> bool {
        self.world.bodies.contains(id.0)
    }

    /// What the caller keeps with a body.
    pub fn user(&self, id: BodyId) -> Option<u64> {
        self.world.bodies.get(id.0).map(|b| b.user_data as u64)
    }

    /// Where a body is: its origin and angles.
    pub fn pose(&self, id: BodyId) -> Option<(Vec3, Vec3)> {
        let body = self.world.bodies.get(id.0)?;
        Some((units(body.translation()), angles(*body.rotation())))
    }

    /// Moves a body there at once, waking it. The ground does not move.
    pub fn set_pose(&mut self, id: BodyId, origin: Vec3, angles: Vec3) {
        if id.0 == self.ground {
            return;
        }
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.set_position(Pose::from_parts(metres(origin), rotation(angles)), true);
        }
        self.part_moved(id);
    }

    /// A body put somewhere has parted from what it is now far enough from: touching it again
    /// is an impact ([`Physics::find_impacts`]).
    fn part_moved(&mut self, id: BodyId) {
        let apart = IMPACT_SEPARATION * METRES_PER_UNIT;
        let mine = self.colliders(id);
        let colliders = &self.world.colliders;
        let parted: Vec<(ColliderHandle, ColliderHandle)> = self
            .together
            .iter()
            .filter(|(a, b)| mine.contains(a) || mine.contains(b))
            .filter(|&&(a, b)| {
                let (Some(a), Some(b)) = (colliders.get(a), colliders.get(b)) else { return true };
                let (pa, pb) = (self.collider_pose(a), self.collider_pose(b));
                rapier3d::parry::query::distance(&pa, a.shape(), &pb, b.shape()).map_or(true, |d| d.distance > apart)
            })
            .copied()
            .collect();
        for pair in parted {
            self.together.remove(&pair);
        }
    }

    /// Units per second.
    pub fn velocity(&self, id: BodyId) -> Vec3 {
        self.world.bodies.get(id.0).map_or([0.0; 3], |b| units(b.linvel()))
    }

    pub fn set_velocity(&mut self, id: BodyId, velocity: Vec3) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.set_linvel(metres(velocity), true);
        }
    }

    /// Degrees per second about the body's own axes, as `GetAngleVelocity` gives them.
    pub fn angular_velocity(&self, id: BodyId) -> Vec3 {
        let Some(body) = self.world.bodies.get(id.0) else { return [0.0; 3] };
        (body.rotation().inverse() * body.angvel()).to_array().map(f32::to_degrees)
    }

    /// Degrees per second about the body's own axes.
    pub fn set_angular_velocity(&mut self, id: BodyId, local: Vec3) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            let world = *body.rotation() * Vector::from(local.map(f32::to_radians));
            body.set_angvel(world, true);
        }
    }

    /// Sets a body going to a pose, as the physics gun drags what it holds: the speeds that
    /// take its origin there and turn it so in `seconds`, no faster than `max_speed` units and
    /// `max_turn` degrees a second. What is in the way still stops it.
    pub fn steer(&mut self, id: BodyId, origin: Vec3, angles: Vec3, seconds: f32, max_speed: f32, max_turn: f32) {
        let Some(body) = self.world.bodies.get_mut(id.0) else { return };
        let seconds = seconds.max(1e-3);
        // the shortest way round from how it is turned to how it should be
        let (axis, mut angle) = (rotation(angles) * body.rotation().inverse()).to_axis_angle();
        if angle > std::f32::consts::PI {
            angle -= std::f32::consts::TAU;
        }
        let turn = (axis * angle / seconds).clamp_length_max(max_turn.to_radians());
        let pose = body.translation();
        let speed = ((metres(origin) - pose) / seconds).clamp_length_max(max_speed * METRES_PER_UNIT);
        // the speed of its origin, and of its centre of mass turning about it
        let centre = body.center_of_mass();
        body.set_linvel(speed + turn.cross(centre - pose), true);
        body.set_angvel(turn, true);
    }

    /// A push through the centre of mass: kilograms times units per second (`ApplyForceCenter`).
    pub fn apply_impulse(&mut self, id: BodyId, impulse: Vec3) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.apply_impulse(metres(impulse), true);
        }
    }

    /// A push at a point of the world, which turns the body too (`ApplyForceOffset`).
    pub fn apply_impulse_at(&mut self, id: BodyId, impulse: Vec3, point: Vec3) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.apply_impulse_at_point(metres(impulse), metres(point), true);
        }
    }

    /// A twist about the world's axes, in kilograms times metres squared times degrees per
    /// second (`ApplyTorqueCenter`).
    pub fn apply_torque_impulse(&mut self, id: BodyId, torque: Vec3) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.apply_torque_impulse(Vector::from(torque.map(f32::to_radians)), true);
        }
    }

    /// Whether it moves: a frozen body stays where it is.
    pub fn motion(&self, id: BodyId) -> bool {
        self.world.bodies.get(id.0).is_some_and(|b| b.is_dynamic())
    }

    /// Freezes a body or lets it move; the ground never moves.
    pub fn set_motion(&mut self, id: BodyId, moves: bool) {
        if id.0 == self.ground {
            return;
        }
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            let kind = if moves { RigidBodyType::Dynamic } else { RigidBodyType::Fixed };
            if body.body_type() != kind {
                body.set_body_type(kind, true);
                if !moves {
                    body.set_linvel(Vector::ZERO, false);
                    body.set_angvel(Vector::ZERO, false);
                }
            }
        }
    }

    pub fn asleep(&self, id: BodyId) -> bool {
        self.world.bodies.get(id.0).is_some_and(|b| b.is_sleeping())
    }

    pub fn wake(&mut self, id: BodyId) {
        self.world.wake_up(id.0, true);
    }

    pub fn sleep(&mut self, id: BodyId) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.sleep();
        }
    }

    pub fn gravity_enabled(&self, id: BodyId) -> bool {
        self.world.bodies.get(id.0).is_some_and(|b| b.gravity_scale() != 0.0)
    }

    pub fn set_gravity_enabled(&mut self, id: BodyId, on: bool) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.set_gravity_scale(if on { 1.0 } else { 0.0 }, true);
        }
    }

    fn colliders(&self, id: BodyId) -> Vec<ColliderHandle> {
        self.world.bodies.get(id.0).map_or_else(Vec::new, |b| b.colliders().to_vec())
    }

    /// Whether it hits anything.
    pub fn collisions(&self, id: BodyId) -> bool {
        self.colliders(id).iter().any(|&c| self.world.colliders.get(c).is_some_and(|c| c.collision_groups() != InteractionGroups::none()))
    }

    /// Whether it hits anything from now on; without, it keeps its mass and falls through all.
    pub fn set_collisions(&mut self, id: BodyId, on: bool) {
        let groups = if on { InteractionGroups::all() } else { InteractionGroups::none() };
        for handle in self.colliders(id) {
            if let Some(collider) = self.world.colliders.get_mut(handle) {
                collider.set_collision_groups(groups);
            }
        }
        self.wake(id);
    }

    /// How slowly a body must move - units a second, at its farthest point - for half a second
    /// to fall asleep: a ragdoll lying against its joints' limits stirs a little, and sleeps
    /// at what VPhysics sleeps.
    pub fn set_sleep_speed(&mut self, id: BodyId, speed: f32) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.activation_mut().normalized_linear_threshold = speed * METRES_PER_UNIT;
        }
    }

    /// Kilograms.
    pub fn mass(&self, id: BodyId) -> f32 {
        self.world.bodies.get(id.0).map_or(0.0, |b| b.mass())
    }

    /// Makes a body this heavy, its mass spread as before.
    pub fn set_mass(&mut self, id: BodyId, mass: f32) {
        let now = self.mass(id);
        if !(mass > 0.0 && now > 0.0) {
            return;
        }
        for handle in self.colliders(id) {
            if let Some(collider) = self.world.colliders.get_mut(handle) {
                collider.set_density(collider.density() * mass / now);
            }
        }
        self.weigh(id.0);
        self.wake(id);
    }

    /// Cubic units.
    pub fn volume(&self, id: BodyId) -> f32 {
        let cubic = METRES_PER_UNIT * METRES_PER_UNIT * METRES_PER_UNIT;
        let colliders = self.colliders(id);
        colliders.iter().filter_map(|&c| self.world.colliders.get(c)).map(|c| c.shape().mass_properties(1.0).mass() / cubic).sum()
    }

    /// How fast a point of the world fixed to the body moves: units per second.
    pub fn velocity_at(&self, id: BodyId, point: Vec3) -> Vec3 {
        self.world.bodies.get(id.0).map_or([0.0; 3], |b| units(b.velocity_at_point(metres(point))))
    }

    /// The centre of mass in the body's own space.
    pub fn mass_center(&self, id: BodyId) -> Vec3 {
        self.world.bodies.get(id.0).map_or([0.0; 3], |b| units(b.local_center_of_mass()))
    }

    /// How fast moving and turning die away, per second, while the body touches something.
    pub fn damping(&self, id: BodyId) -> (f32, f32) {
        self.damping.get(&id.0).copied().unwrap_or((0.0, 0.0))
    }

    /// How fast moving and turning die away from now on, per second: as IVP damps, only while
    /// the body touches something (in GMod a crate in the air damped by 100 turns on as one not
    /// damped; one sliding on the ground damped by 5 keeps e⁻⁵ of its speed a second).
    pub fn set_damping(&mut self, id: BodyId, linear: f32, angular: f32) {
        if !self.world.bodies.contains(id.0) {
            return;
        }
        let damping = (linear.max(0.0), angular.max(0.0));
        if damping == (0.0, 0.0) {
            self.damping.remove(&id.0);
        } else {
            self.damping.insert(id.0, damping);
        }
        self.damp_touching();
    }

    /// Before a step: each body with damping is damped if it touches something, and not if it
    /// does not.
    fn damp_touching(&mut self) {
        let bodies = &mut self.world.bodies;
        self.damping.retain(|&handle, _| bodies.contains(handle));
        for (&handle, &(linear, angular)) in &self.damping {
            let Some(body) = self.world.bodies.get(handle) else { continue };
            let narrow = &self.world.narrow_phase;
            let touches = body.colliders().iter().any(|&c| narrow.contact_pairs_with(c).any(|pair| pair.has_any_active_contact()));
            let (linear, angular) = if touches { (linear, angular) } else { (0.0, 0.0) };
            if let Some(body) = self.world.bodies.get_mut(handle)
                && (body.linear_damping(), body.angular_damping()) != (linear, angular)
            {
                body.set_linear_damping(linear);
                body.set_angular_damping(angular);
            }
        }
    }

    /// Where a collider is now: on its body as the body is. A collider follows its body only
    /// when the world steps, so one whose body was just moved ([`Physics::set_pose`]) still
    /// says where it was.
    fn collider_pose(&self, collider: &Collider) -> Pose {
        match (collider.parent().and_then(|b| self.world.bodies.get(b)), collider.position_wrt_parent()) {
            (Some(body), Some(local)) => *body.position() * *local,
            _ => *collider.position(),
        }
    }

    /// The box around a body in the world, where it is now.
    pub fn bounds(&self, id: BodyId) -> Option<(Vec3, Vec3)> {
        let mut found: Option<(Vector, Vector)> = None;
        for handle in self.colliders(id) {
            let Some(collider) = self.world.colliders.get(handle) else { continue };
            let aabb = collider.shape().compute_aabb(&self.collider_pose(collider));
            found = Some(found.map_or((aabb.mins, aabb.maxs), |(lo, hi)| (lo.min(aabb.mins), hi.max(aabb.maxs))));
        }
        found.map(|(lo, hi)| (units(lo), units(hi)))
    }

    /// The first body a ray from `start` to `end` hits, past `skip`; the world is not looked at.
    /// Bodies are found where the last step left them, not where they were moved since
    /// ([`Physics::ray_into`] finds one where it is now).
    pub fn ray(&self, start: Vec3, end: Vec3, skip: Option<BodyId>) -> Option<Hit> {
        let (from, to) = (metres(start), metres(end));
        let length = (to - from).length();
        if length <= 0.0 {
            return None;
        }
        let ray = Ray::new(from, (to - from) / length);
        let bodies_only = |_: ColliderHandle, c: &Collider| c.parent().is_some();
        let mut filter = QueryFilter::new().predicate(&bodies_only);
        if let Some(skip) = skip {
            filter = filter.exclude_rigid_body(skip.0);
        }
        let (collider, hit) = self.world.cast_ray_and_get_normal(&ray, length, true, filter)?;
        let body = self.world.colliders.get(collider)?.parent()?;
        Some(Hit { body: BodyId(body), fraction: hit.time_of_impact / length, normal: hit.normal.to_array() })
    }

    /// Whether a ray from `start` to `end` hits one body, and where; the world and other
    /// bodies are not looked at. The body is found where it is now.
    pub fn ray_into(&self, start: Vec3, end: Vec3, id: BodyId) -> Option<Hit> {
        let (from, to) = (metres(start), metres(end));
        let length = (to - from).length();
        if length <= 0.0 {
            return None;
        }
        let ray = Ray::new(from, (to - from) / length);
        let hits = self.colliders(id).into_iter().filter_map(|handle| {
            let collider = self.world.colliders.get(handle)?;
            collider.shape().cast_ray_and_get_normal(&self.collider_pose(collider), &ray, length, true)
        });
        let hit = hits.min_by(|a, b| a.time_of_impact.total_cmp(&b.time_of_impact))?;
        Some(Hit { body: id, fraction: hit.time_of_impact / length, normal: hit.normal.to_array() })
    }

    /// Whether two bodies are in each other where they are now.
    pub fn overlapping(&self, a: BodyId, b: BodyId) -> bool {
        let shapes = |id: BodyId| self.colliders(id).into_iter().filter_map(|h| self.world.colliders.get(h)).collect::<Vec<_>>();
        let (a, b) = (shapes(a), shapes(b));
        a.iter().any(|x| {
            b.iter().any(|y| {
                let hit = rapier3d::parry::query::intersection_test(&self.collider_pose(x), x.shape(), &self.collider_pose(y), y.shape());
                hit.is_ok_and(|hit| hit.intersecting)
            })
        })
    }

    /// Whether two bodies go through each other from now on, joined or not: what keeps the
    /// parts of a ragdoll from pushing each other apart.
    pub fn set_apart(&mut self, a: BodyId, b: BodyId, apart: bool) {
        if a == b {
            return;
        }
        if apart {
            self.apart.insert((a.0, b.0));
            self.apart.insert((b.0, a.0));
            // only colliders that ask are filtered
            for handle in self.colliders(a).into_iter().chain(self.colliders(b)) {
                if let Some(collider) = self.world.colliders.get_mut(handle) {
                    collider.set_active_hooks(collider.active_hooks() | ActiveHooks::FILTER_CONTACT_PAIRS);
                }
            }
        } else {
            self.apart.remove(&(a.0, b.0));
            self.apart.remove(&(b.0, a.0));
        }
        self.wake(a);
        self.wake(b);
    }

    /// Whether a body goes through every other that moves from now on, as if it were alone with
    /// the world and what does not move: those that move do not push it or feel it. Two such
    /// bodies still hit each other.
    pub fn set_aloof(&mut self, id: BodyId, aloof: bool) {
        if id.0 == self.ground {
            return;
        }
        if aloof {
            self.aloof.insert(id.0);
            for handle in self.colliders(id) {
                if let Some(collider) = self.world.colliders.get_mut(handle) {
                    collider.set_active_hooks(collider.active_hooks() | ActiveHooks::FILTER_CONTACT_PAIRS);
                }
            }
        } else {
            self.aloof.remove(&id.0);
        }
        self.wake(id);
    }

    /// What says whether bodies of two collision groups collide (the game's rules).
    pub fn set_group_rule(&mut self, rule: fn(i64, i64) -> bool) {
        self.group_rule = Some(rule);
    }

    /// A body's collision group from now on (`COLLISION_GROUP_*`; 0 is none): bodies of two
    /// groups collide as [`Physics::set_group_rule`] says, and [`WORLD_ONLY`] collides with
    /// what never moves only - the map.
    pub fn set_collision_group(&mut self, id: BodyId, group: i64) {
        if id.0 == self.ground {
            return;
        }
        if group == 0 {
            self.groups.remove(&id.0);
        } else {
            self.groups.insert(id.0, group);
            for handle in self.colliders(id) {
                if let Some(collider) = self.world.colliders.get_mut(handle) {
                    collider.set_active_hooks(collider.active_hooks() | ActiveHooks::FILTER_CONTACT_PAIRS);
                }
            }
        }
        self.wake(id);
    }

    /// Whether two bodies go through each other ([`Physics::set_apart`]).
    pub fn apart(&self, a: BodyId, b: BodyId) -> bool {
        self.apart.contains(&(a.0, b.0))
    }

    /// The bodies that move and are awake.
    pub fn awake(&self) -> Vec<BodyId> {
        self.world.active_bodies().map(|(handle, _)| BodyId(handle)).collect()
    }

    /// Joins two bodies, as they are now. They go through each other unless `collide`. A joint
    /// with a strength - the kilograms whose weight breaks it, and the kilograms whose weight
    /// on a lever of a unit twists it apart; 0 for never - breaks when pulled harder
    /// ([`Physics::broken`]). None when a body is not there, or a rope or spring has no length.
    pub fn join(&mut self, a: BodyId, b: BodyId, joint: Joint, collide: bool, strength: [f32; 2]) -> Option<JointId> {
        let first = *self.world.bodies.get(a.0)?.position();
        let second = *self.world.bodies.get(b.0)?.position();
        // a frame of the world, as each body sees it
        let frames = |builder: GenericJointBuilder, frame: Pose| {
            builder.local_frame1(first.inverse() * frame).local_frame2(second.inverse() * frame)
        };
        let along = |point: Vec3, axis: Vec3| {
            let axis = Vector::from(axis).normalize_or(Vector::X);
            Pose::from_parts(metres(point), Rotation::from_rotation_arc(Vector::X, axis))
        };
        let at = |point: Vec3| Pose::from_parts(metres(point), Rotation::IDENTITY);
        let mut held = None;
        let data: GenericJoint = match joint {
            Joint::Weld => frames(GenericJointBuilder::new(JointAxesMask::LOCKED_FIXED_AXES), first).build(),
            Joint::Ball { point } => frames(GenericJointBuilder::new(JointAxesMask::LOCKED_SPHERICAL_AXES), at(point)).build(),
            Joint::Hinge { point, axis } => {
                frames(GenericJointBuilder::new(JointAxesMask::LOCKED_REVOLUTE_AXES), along(point, axis)).build()
            }
            Joint::Slider { point, axis } => {
                frames(GenericJointBuilder::new(JointAxesMask::LOCKED_PRISMATIC_AXES), along(point, axis)).build()
            }
            Joint::Rope { start, end, length, rigid } => {
                let length = length * METRES_PER_UNIT;
                if length <= 0.0 {
                    return None;
                }
                let builder = GenericJointBuilder::new(JointAxesMask::empty())
                    .coupled_axes(JointAxesMask::LIN_AXES)
                    .limits(JointAxis::LinX, [if rigid { length } else { 0.0 }, length])
                    .local_anchor1(first.inverse_transform_point(metres(start)))
                    .local_anchor2(second.inverse_transform_point(metres(end)));
                // the solver keeps coupled axes from going past their greatest length only: a
                // rod also holds them apart, by a motor as stiff as a joint can be
                let builder = if rigid {
                    builder
                        .motor_position(JointAxis::LinX, length, ROD_STIFFNESS, ROD_DAMPING)
                        .motor_model(JointAxis::LinX, rapier3d::prelude::MotorModel::AccelerationBased)
                } else {
                    builder
                };
                builder.build()
            }
            Joint::Spring { start, end, length, stiffness, damping, .. } => {
                SpringJointBuilder::new((length * METRES_PER_UNIT).max(0.0), stiffness.max(0.0), damping.max(0.0))
                    .local_anchor1(first.inverse_transform_point(metres(start)))
                    .local_anchor2(second.inverse_transform_point(metres(end)))
                    .build()
                    .data
            }
            Joint::NoCollide => GenericJointBuilder::new(JointAxesMask::empty()).build(),
            Joint::Ragdoll { point, angles, limits, free } => {
                // the limits are set anew before every step ([`Physics::aim_limits`])
                let ranges = limits.map(|[a, b]| [a.min(b), a.max(b)].map(|l| l.clamp(-180.0, 180.0).to_radians()));
                // limits of a hundredth of a degree (constraint.AdvBallsocket's way to say "not at
                // all", as simfphys holds its wheels) hold as VPhysics holds them: fast
                let still = ranges.iter().all(|r| r[0].abs() < STILL_LIMIT && r[1].abs() < STILL_LIMIT);
                if !still {
                    held = Some(ranges);
                }
                // what may not turn at all is welded, but where a free one goes
                let mask = match (still, free) {
                    (true, false) => JointAxesMask::LOCKED_FIXED_AXES,
                    (true, true) => JointAxesMask::ANG_AXES,
                    (false, false) => JointAxesMask::LOCKED_SPHERICAL_AXES,
                    (false, true) => JointAxesMask::empty(),
                };
                frames(GenericJointBuilder::new(mask), Pose::from_parts(metres(point), rotation(angles))).build()
            }
        };
        let mut data = data;
        data.set_contacts_enabled(collide && joint != Joint::NoCollide);
        // the two wake now rather than at the next step, so that one put to sleep after
        // (a ragdoll that starts asleep) stays asleep
        let id = JointId(self.world.impulse_joints.insert(a.0, b.0, data, false));
        self.wake(a);
        self.wake(b);
        if let Some(ranges) = held {
            self.limits.push(Limit { joint: id, frame: data.local_frame1, ranges });
        }
        if let Joint::Spring { relative_damping, .. } = joint
            && relative_damping > 0.0
        {
            self.dampers.insert(id, relative_damping);
        }
        if let Joint::Spring { length, stiffness, damping, stretch_only: true, .. } = joint {
            self.slack.insert(id, [(length * METRES_PER_UNIT).max(0.0), stiffness.max(0.0), damping.max(0.0)]);
        }
        // the weight of a kilogram as the game falls, on a lever of a unit
        let weight = DEFAULT_GRAVITY * METRES_PER_UNIT;
        let [force, torque] = strength;
        if force > 0.0 || torque > 0.0 {
            self.strengths.insert(id, [force.max(0.0) * weight, torque.max(0.0) * weight * METRES_PER_UNIT]);
        }
        Some(id)
    }

    /// Takes a joint away.
    pub fn unjoin(&mut self, id: JointId) {
        self.world.remove_impulse_joint(id.0);
        self.strengths.remove(&id);
        self.dampers.remove(&id);
        self.slack.remove(&id);
        self.limits.retain(|limit| limit.joint != id);
    }

    /// How far apart the two points of a joint are, metres.
    fn joint_length(&self, id: JointId) -> Option<f32> {
        let joint = self.world.impulse_joints.get(id.0)?;
        let (a, b) = (self.world.bodies.get(joint.body1())?, self.world.bodies.get(joint.body2())?);
        Some(((*a.position() * joint.data.local_frame1.translation) - (*b.position() * joint.data.local_frame2.translation)).length())
    }

    /// Before a step: the springs that only stretch do nothing while their points are nearer
    /// than their length.
    fn slacken_springs(&mut self) {
        let joints = &self.world.impulse_joints;
        self.slack.retain(|id, _| joints.get(id.0).is_some());
        let slack: Vec<(JointId, [f32; 3])> = self.slack.iter().map(|(&id, &how)| (id, how)).collect();
        for (id, [length, stiffness, damping]) in slack {
            let Some(now) = self.joint_length(id) else { continue };
            // a motor of no stiffness still holds its speed: one of no force holds nothing
            let force = if now < length { 0.0 } else { f32::MAX };
            if let Some(joint) = self.world.impulse_joints.get_mut(id.0, false) {
                joint.data.set_motor_position(JointAxis::LinX, length, stiffness, damping);
                joint.data.set_motor_max_force(JointAxis::LinX, force);
            }
        }
    }

    /// Before a step: each spring's relative damping takes from how fast its two points move
    /// apart, any way, what `seconds` of it would - all of it, however strong, never more.
    fn damp_springs(&mut self, seconds: f32) {
        let joints = &self.world.impulse_joints;
        self.dampers.retain(|id, _| joints.get(id.0).is_some());
        for (id, &damping) in &self.dampers {
            // slack, it holds nothing back
            if self.slack.get(id).is_some_and(|&[length, ..]| self.joint_length(*id).is_some_and(|now| now < length)) {
                continue;
            }
            let Some(joint) = self.world.impulse_joints.get(id.0) else { continue };
            let (one, two) = (joint.body1(), joint.body2());
            let (frame1, frame2) = (joint.data.local_frame1, joint.data.local_frame2);
            let (Some(a), Some(b)) = (self.world.bodies.get(one), self.world.bodies.get(two)) else { continue };
            if a.is_sleeping() && b.is_sleeping() {
                continue;
            }
            let (pa, pb) = (*a.position() * frame1.translation, *b.position() * frame2.translation);
            let relative = b.velocity_at_point(pb) - a.velocity_at_point(pa);
            if relative.length_squared() < 1e-12 {
                continue;
            }
            // how much a push at the point along the way it moves changes its speed there: by
            // the body's mass and, off its middle, its turning too - what moves, that is: a
            // fixed body or one moved by the game takes nothing
            let way = relative.normalize();
            let inverse = |body: &rapier3d::prelude::RigidBody, point: Vector| {
                if !body.is_dynamic() {
                    return 0.0;
                }
                let props = body.mass_properties();
                let arm = point - props.world_com;
                let turn = props.effective_world_inv_inertia * arm.cross(way);
                1.0 / body.mass().max(1e-6) + way.dot(turn.cross(arm))
            };
            let inverse_mass = inverse(a, pa) + inverse(b, pb);
            if inverse_mass <= 0.0 {
                continue;
            }
            let taken = 1.0 - (-damping * seconds * inverse_mass).exp();
            let impulse = relative * (taken / inverse_mass);
            if let Some(a) = self.world.bodies.get_mut(one)
                && a.is_dynamic()
            {
                a.apply_impulse_at_point(impulse, pa, true);
            }
            if let Some(b) = self.world.bodies.get_mut(two)
                && b.is_dynamic()
            {
                b.apply_impulse_at_point(-impulse, pb, true);
            }
        }
    }

    /// Sets the limits of the ragdoll joints for the step to come ([`Joint::Ragdoll`]). A turn
    /// splits into a twist about the frame's x and a swing - a turn about an axis across x -
    /// whose parts along y and z are limited each. The solver measures the turn about each
    /// axis from the joint's frame, which holds near it but mixes the axes up as a joint
    /// bends far, as knees and elbows do; so the frame is moved to where the second body is
    /// turned now, and each axis limited to what is left of its range from there.
    fn aim_limits(&mut self) {
        let joints = &self.world.impulse_joints;
        self.limits.retain(|limit| joints.get(limit.joint.0).is_some());
        for limit in &self.limits {
            let Some(joint) = self.world.impulse_joints.get(limit.joint.0) else { continue };
            let (Some(first), Some(second)) = (self.world.bodies.get(joint.body1()), self.world.bodies.get(joint.body2())) else {
                continue;
            };
            if first.is_sleeping() && second.is_sleeping() {
                continue;
            }
            let turn = (*first.position() * limit.frame).rotation.inverse() * (*second.position() * joint.data.local_frame2).rotation;
            let now = split(turn);
            let Some(joint) = self.world.impulse_joints.get_mut(limit.joint.0, false) else { continue };
            joint.data.set_local_frame1(Pose::from_parts(limit.frame.translation, (limit.frame.rotation * turn).normalize()));
            for (i, axis) in [JointAxis::AngX, JointAxis::AngY, JointAxis::AngZ].into_iter().enumerate() {
                let [least, most] = limit.ranges[i];
                // all the way round is no limit
                if most - least < std::f32::consts::TAU - 0.02 {
                    joint.data.set_limits(axis, [least - now[i], most - now[i]]);
                }
            }
        }
    }

    /// Whether a joint holds: it was not taken away, broken, or lost with a body.
    pub fn joined(&self, id: JointId) -> bool {
        self.world.impulse_joints.get(id.0).is_some()
    }

    /// The joints that broke since this was last asked.
    pub fn broken(&mut self) -> Vec<JointId> {
        std::mem::take(&mut self.broken)
    }

    /// What a body touches now: one contact for each thing, where they touch deepest.
    pub fn contacts(&self, id: BodyId) -> Vec<Contact> {
        let world = &self.world;
        let Some(body) = world.bodies.get(id.0) else { return Vec::new() };
        let mut out = Vec::new();
        for &collider in body.colliders() {
            for pair in world.narrow_phase.contact_pairs_with(collider) {
                if !pair.has_any_active_contact() {
                    continue;
                }
                let ours = pair.collider1 == collider;
                let other_collider = if ours { pair.collider2 } else { pair.collider1 };
                let other = world.colliders.get(other_collider).and_then(|c| c.parent()).filter(|&h| h != self.ground);
                let Some((manifold, contact)) = pair.find_deepest_contact() else { continue };
                let point = world.colliders.get(pair.collider1).map(|c| units(c.position() * contact.local_p1));
                let normal = if ours { manifold.data.normal } else { -manifold.data.normal };
                let impulse: f32 = pair.manifolds().iter().flat_map(|m| m.points.iter()).map(|p| p.data.impulse).sum();
                let step = self.world.integration_parameters.dt.max(1e-6);
                out.push(Contact {
                    other: other.map(BodyId),
                    point: point.unwrap_or_else(|| units(body.translation())),
                    normal: normal.to_array(),
                    force: impulse / step / METRES_PER_UNIT,
                });
            }
        }
        out
    }

    /// The impacts since this was last asked, oldest first.
    pub fn impacts(&mut self) -> Vec<Impact> {
        std::mem::take(&mut self.impacts)
    }

    /// The impacts of the step just made, as IVP makes them: two colliders that had been apart
    /// touching again, however slowly; touching, they make no other until they part. Each of
    /// the two bodies that moves has one, at how fast they came together.
    fn find_impacts(&mut self, before: &HashMap<RigidBodyHandle, (Vector, Vector, Vector)>) {
        let world = &self.world;
        let apart = IMPACT_SEPARATION * METRES_PER_UNIT;
        let mut near = HashSet::new();
        // the world is one: what touches two of its solids at once hits it once
        let mut hit = HashSet::new();
        for pair in world.narrow_phase.contact_pairs() {
            let key = (pair.collider1, pair.collider2);
            let (Some(collider1), Some(collider2)) = (world.colliders.get(pair.collider1), world.colliders.get(pair.collider2)) else {
                continue;
            };
            let touching = pair.has_any_active_contact() && pair.total_impulse_magnitude() > 0.0;
            if self.together.contains(&key) {
                // touching still, or not yet far enough apart
                let parted = !touching
                    && rapier3d::parry::query::distance(collider1.position(), collider1.shape(), collider2.position(), collider2.shape())
                        .map_or(true, |d| d.distance > apart);
                if !parted {
                    near.insert(key);
                }
                continue;
            }
            if !touching {
                continue;
            }
            near.insert(key);
            let (_, normal) = pair.max_impulse();
            let point = pair.find_deepest_contact().map(|(_, contact)| collider1.position() * contact.local_p1);
            let body = |collider: &Collider| collider.parent();
            let (one, two) = (body(collider1), body(collider2));
            let at = point.unwrap_or_else(|| collider1.position().translation);
            // how fast they came together there
            let speed_at = |handle: Option<RigidBodyHandle>| -> Vector {
                let Some(handle) = handle else { return Vector::ZERO };
                match before.get(&handle) {
                    Some(&(v, w, com)) => v + w.cross(at - com),
                    None => world.bodies.get(handle).map_or(Vector::ZERO, |b| b.velocity_at_point(at)),
                }
            };
            let speed = (normal.dot(speed_at(one) - speed_at(two)) / METRES_PER_UNIT).max(0.0);
            let bodies =
                if one.map(RigidBodyHandle::into_raw_parts) <= two.map(RigidBodyHandle::into_raw_parts) { (one, two) } else { (two, one) };
            if !hit.insert(bodies) {
                continue;
            }
            let since = self.last_impacts.insert(bodies, self.clock).map_or(1.0, |t| (self.clock - t).min(1.0));
            let going = |handle: Option<RigidBodyHandle>| match handle.map(|h| (h, before.get(&h))) {
                Some((_, Some(&(v, _, _)))) => units(v),
                Some((h, None)) => world.bodies.get(h).map_or([0.0; 3], |b| units(b.linvel())),
                None => [0.0; 3],
            };
            for (me, other, towards) in [(one, two, normal), (two, one, -normal)] {
                let Some(handle) = me else { continue };
                let Some(rb) = world.bodies.get(handle) else { continue };
                if !rb.is_dynamic() || rb.mass() <= 0.0 {
                    continue;
                }
                // the world's solids have no body; joints' ground is no thing to hit
                let other = other.filter(|&h| h != self.ground);
                self.impacts.push(Impact {
                    body: BodyId(handle),
                    user: rb.user_data as u64,
                    other_body: other.map(BodyId),
                    other: other.and_then(|h| world.bodies.get(h)).map(|b| b.user_data as u64),
                    speed,
                    point: units(at),
                    normal: towards.to_array(),
                    velocity: going(me),
                    other_velocity: going(other),
                    since,
                });
            }
        }
        // what is no longer near anything has parted
        self.together = near;
        let clock = self.clock;
        self.last_impacts.retain(|_, t| clock - *t < 1.0);
    }

    /// How each body that moves is going: its speed, its turning, and where its mass is.
    fn speeds(&self) -> HashMap<RigidBodyHandle, (Vector, Vector, Vector)> {
        self.world.active_bodies().map(|(handle, b)| (handle, (b.linvel(), b.angvel(), b.center_of_mass()))).collect()
    }

    /// Bounces what hit something in the last step, as IVP does an impact: the solver stopped
    /// it, and one push in the middle of where they touch sends the two apart as fast as they
    /// came together times how much they bounce. A flat face landing flat bounces flat (the
    /// solver's own bounce, point by point, would set it turning).
    fn bounce_impacts(&mut self, before: &HashMap<RigidBodyHandle, (Vector, Vector, Vector)>) {
        let least = LEAST_BOUNCING_SPEED * METRES_PER_UNIT;
        let mut pushes = Vec::new();
        for pair in self.world.narrow_phase.contact_pairs() {
            if !pair.has_any_active_contact() {
                continue;
            }
            let Some(collider1) = self.world.colliders.get(pair.collider1) else { continue };
            for manifold in pair.manifolds() {
                // where they touch: the points of the face that touches, all of them (not only
                // those the solver pushed on, which may be one side of it)
                if manifold.points.iter().all(|p| p.data.impulse <= 0.0) {
                    continue;
                }
                let nearest = manifold.points.iter().map(|p| p.dist).fold(f32::MAX, f32::min);
                let face = manifold.points.iter().filter(|p| p.dist <= nearest + 0.1 * METRES_PER_UNIT);
                let touching: Vec<Vector> = face.map(|p| collider1.position() * p.local_p1).collect();
                let normal = manifold.data.normal;
                let keep = corners(&touching, normal);
                let touching: Vec<Vector> = touching.iter().zip(keep).filter(|&(_, k)| k).map(|(&p, _)| p).collect();
                let point = touching.iter().copied().sum::<Vector>() / touching.len().max(1) as f32;
                let bodies = [manifold.data.rigid_body1, manifold.data.rigid_body2];
                // how fast they came together, and how fast they part now
                let speed_at = |handle: Option<RigidBodyHandle>, then: bool| -> Vector {
                    let Some(handle) = handle else { return Vector::ZERO };
                    match (then, before.get(&handle)) {
                        (true, Some(&(v, w, com))) => v + w.cross(point - com),
                        _ => self.world.bodies.get(handle).map_or(Vector::ZERO, |b| b.velocity_at_point(point)),
                    }
                };
                let came = normal.dot(speed_at(bodies[1], true) - speed_at(bodies[0], true));
                if came > -least {
                    continue;
                }
                let parts = normal.dot(speed_at(bodies[1], false) - speed_at(bodies[0], false));
                let subshapes = [manifold.subshape1, manifold.subshape2];
                let (_, e) = contact_material(&self.made_of, &self.world.colliders, [pair.collider1, pair.collider2], normal, subshapes);
                let more = -e * came - parts;
                if more <= 0.0 {
                    continue;
                }
                // how much a push there along the normal changes that
                let give = |handle: Option<RigidBodyHandle>| -> f32 {
                    let Some(b) = handle.and_then(|h| self.world.bodies.get(h)).filter(|b| b.is_dynamic()) else { return 0.0 };
                    let props = b.mass_properties();
                    let arm = (point - b.center_of_mass()).cross(normal);
                    normal.dot(props.effective_inv_mass * normal) + arm.dot(props.effective_world_inv_inertia * arm)
                };
                let k = give(bodies[0]) + give(bodies[1]);
                if k > 0.0 {
                    pushes.push((bodies, normal * (more / k), point));
                }
            }
        }
        for ([one, two], impulse, point) in pushes {
            for (handle, impulse) in [(one, -impulse), (two, impulse)] {
                if let Some(b) = handle.and_then(|h| self.world.bodies.get_mut(h)).filter(|b| b.is_dynamic()) {
                    b.apply_impulse_at_point(impulse, point, true);
                }
            }
        }
    }

    /// Moves time on by `seconds`. Joints pulled harder than they take break.
    pub fn step(&mut self, seconds: f32) {
        if seconds <= 0.0 {
            return;
        }
        self.world.integration_parameters.dt = seconds;
        self.aim_limits();
        self.slacken_springs();
        self.damp_touching();
        self.damp_springs(seconds);
        let hooks = Apart {
            pairs: &self.apart,
            aloof: &self.aloof,
            groups: &self.groups,
            rule: self.group_rule,
            seams: &self.seams,
            made_of: &self.made_of,
        };
        let before = self.speeds();
        self.world.step_with_events(&hooks, &());
        self.clock += seconds;
        self.bounce_impacts(&before);
        self.limit_speeds();
        self.find_impacts(&before);
        let substep = seconds / self.world.integration_parameters.num_solver_iterations.max(1) as f32;
        let joints = &self.world.impulse_joints;
        let mut snapped = Vec::new();
        self.strengths.retain(|id, [force, torque]| {
            let Some(joint) = joints.get(id.0) else { return false };
            // the impulses of the last of the solver's substeps
            let i = joint.impulses;
            let pulled = Vector::new(i[0], i[1], i[2]).length() / substep;
            let twisted = Vector::new(i[3], i[4], i[5]).length() / substep;
            let breaks = (*force > 0.0 && pulled > *force) || (*torque > 0.0 && twisted > *torque);
            if breaks {
                snapped.push(*id);
            }
            !breaks
        });
        for id in snapped {
            self.world.remove_impulse_joint(id.0);
            self.broken.push(id);
        }
    }
}

impl Physics {
    /// No body faster than the limits ([`Physics::set_speed_limits`]), as VPhysics keeps them.
    fn limit_speeds(&mut self) {
        let [speed, turn] = self.speed_limits;
        let (speed, turn) = (speed * METRES_PER_UNIT, turn.to_radians());
        let active: Vec<RigidBodyHandle> = self.world.active_bodies().map(|(handle, _)| handle).collect();
        for handle in active {
            let Some(body) = self.world.bodies.get_mut(handle) else { continue };
            let (v, w) = (body.linvel(), body.angvel());
            if speed > 0.0 && v.length() > speed {
                body.set_linvel(v * (speed / v.length()), false);
            }
            if turn > 0.0 && w.length() > turn {
                body.set_angvel(w * (turn / w.length()), false);
            }
        }
    }
}

/// What a ragdoll joint turns within ([`Physics::aim_limits`]).
#[derive(Debug, Clone, Copy)]
struct Limit {
    joint: JointId,
    /// Its frame in the first body, where the second turns from.
    frame: Pose,
    /// Radians: the twist about x, the swing about y and about z.
    ranges: [[f32; 2]; 3],
}

/// A turn as the twist about x and the parts along y and z of the swing after it, radians.
fn split(turn: Rotation) -> [f32; 3] {
    let turn = if turn.w < 0.0 { -turn } else { turn };
    let twist = Rotation::from_xyzw(turn.x, 0.0, 0.0, turn.w);
    let twist = if twist.length() > 1e-6 { twist.normalize() } else { Rotation::IDENTITY };
    let swing = (turn * twist.inverse()).to_scaled_axis();
    [2.0 * twist.x.atan2(twist.w), swing.y, swing.z]
}

/// What keeps the bodies set apart, and the aloof ones, from touching.
struct Apart<'a> {
    pairs: &'a HashSet<(RigidBodyHandle, RigidBodyHandle)>,
    aloof: &'a HashSet<RigidBodyHandle>,
    groups: &'a HashMap<RigidBodyHandle, i64>,
    rule: Option<fn(i64, i64) -> bool>,
    seams: &'a HashMap<ColliderHandle, Vec<ColliderHandle>>,
    made_of: &'a HashMap<ColliderHandle, MadeOf>,
}

/// `COLLISION_GROUP_WORLD`: what collides with the map only.
pub const WORLD_ONLY: i64 = 20;

impl PhysicsHooks for Apart<'_> {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        let (a, b) = (context.rigid_body1, context.rigid_body2);
        let aloof = |h: Option<RigidBodyHandle>| h.is_some_and(|h| self.aloof.contains(&h));
        let moves = |h: Option<RigidBodyHandle>| h.and_then(|h| context.bodies.get(h)).is_some_and(|b| b.is_dynamic());
        // the collision groups: what has none is the map's or `COLLISION_GROUP_NONE`
        let group = |h: Option<RigidBodyHandle>| h.and_then(|h| self.groups.get(&h).copied()).unwrap_or(0);
        let (ga, gb) = (group(a), group(b));
        if ga != 0 || gb != 0 {
            let map = |h: Option<RigidBodyHandle>| h.is_none();
            if (ga == WORLD_ONLY && !map(b)) || (gb == WORLD_ONLY && !map(a)) {
                return None;
            }
            if !map(a) && !map(b) && self.rule.is_some_and(|rule| !rule(ga, gb)) {
                return None;
            }
        }
        match (a, b) {
            (Some(a), Some(b)) if self.pairs.contains(&(a, b)) => None,
            _ if aloof(a) != aloof(b) && (aloof(a) && moves(b) || aloof(b) && moves(a)) => None,
            _ => Some(SolverFlags::COMPUTE_RIGID_IMPULSES),
        }
    }

    /// A contact with a solid of the world of many surfaces is of the one touched. A contact
    /// with a solid of the world on a face it shares with a neighbour - one a step out of it
    /// along the contact is inside that neighbour - is no contact: the face is inside the
    /// world, between two brushes, and what slides over their seam would trip on its edge.
    /// IVP's world is one surface and has no such faces.
    fn modify_solver_contacts(&self, context: &mut ContactModificationContext) {
        let colliders = context.colliders;
        if let ModifiableContacts::Rigid(manifold) = &mut context.contacts {
            let pair = [context.collider1, context.collider2];
            let subshapes = [manifold.manifold.subshape1, manifold.manifold.subshape2];
            if let (Some(friction), _) = contact_material(self.made_of, colliders, pair, *manifold.normal, subshapes) {
                *manifold.friction = friction;
            }
            // the solver stops what hits; it bounces after ([`Physics::bounce_impacts`])
            *manifold.restitution = 0.0;
            if manifold.solver_contacts.len() > 3 {
                let points: Vec<Vector> = manifold.solver_contacts.iter().map(|c| c.anchor1).collect();
                let mut keep = corners(&points, *manifold.normal).into_iter();
                manifold.solver_contacts.retain(|_| keep.next().unwrap_or(true));
            }
        }
        let (solid, first) = match (self.seams.get(&context.collider1), self.seams.get(&context.collider2)) {
            (Some(_), _) => (context.collider1, true),
            (None, Some(_)) => (context.collider2, false),
            _ => return,
        };
        let Some(neighbours) = self.seams.get(&solid) else { return };
        let ModifiableContacts::Rigid(manifold) = &mut context.contacts else { return };
        let out = if first { *manifold.normal } else { -*manifold.normal };
        manifold.solver_contacts.retain(|c| {
            let on_solid = if first { c.anchor1 } else { c.anchor2 };
            let probe = on_solid + out * SEAM_PROBE;
            !neighbours.iter().any(|&n| colliders.get(n).is_some_and(|other| other.shape().contains_point(other.position(), probe)))
        });
    }
}

#[cfg(test)]
mod tests;
