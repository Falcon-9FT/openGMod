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
    ActiveHooks, Collider, ColliderBuilder, ColliderHandle, GenericJoint, GenericJointBuilder, ImpulseJointHandle, InteractionGroups,
    JointAxesMask, JointAxis, MassProperties, PairFilterContext, PhysicsHooks, PhysicsWorld, QueryFilter, Ray, RigidBodyBuilder,
    RigidBodyHandle, RigidBodyType, SharedShape, SolverFlags, SpringJointBuilder,
};

pub type Vec3 = [f32; 3];

/// A unit in metres.
pub const METRES_PER_UNIT: f32 = 0.0254;
/// `sv_gravity`: units/s² down.
pub const DEFAULT_GRAVITY: f32 = 600.0;
/// Kilograms in a cubic metre of what has no mass of its own: the `default` surface property.
pub const DEFAULT_DENSITY: f32 = 2000.0;
/// How much what slides along something holds back, when nothing says otherwise.
pub const DEFAULT_FRICTION: f32 = 0.8;
/// The least change of speed a contact makes to count as an [`Impact`], in units/s: less is
/// a body at rest, or one sliding along.
pub const MIN_IMPACT_SPEED: f32 = 50.0;
/// How long, in seconds, after an impact a body makes no other.
const IMPACT_PAUSE: f32 = 0.08;

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
    /// and `rotdamping`).
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
    /// second squared) and `damping` (kilograms per second).
    Spring { start: Vec3, end: Vec3, length: f32, stiffness: f32, damping: f32 },
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
    /// Impacts since they were last asked for, and when each body last made one.
    impacts: Vec<Impact>,
    last_impact: HashMap<RigidBodyHandle, f32>,
    /// The ragdoll joints and their limits.
    limits: Vec<Limit>,
    /// The bodies harder to turn than their shapes ([`Body::inertia`]), and how much.
    inertia: HashMap<RigidBodyHandle, f32>,
    /// Seconds stepped.
    clock: f32,
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
    /// How much the contact changed its speed, in units/s: how fast it went into what it hit.
    pub speed: f32,
    /// Where, in the world.
    pub point: Vec3,
    /// From the body towards what it hit.
    pub normal: Vec3,
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
        let ground = world.insert_body(RigidBodyBuilder::fixed());
        Physics {
            world,
            ground,
            strengths: HashMap::new(),
            broken: Vec::new(),
            apart: HashSet::new(),
            aloof: HashSet::new(),
            impacts: Vec::new(),
            last_impact: HashMap::new(),
            limits: Vec::new(),
            inertia: HashMap::new(),
            clock: 0.0,
        }
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
        let mut added = 0;
        for corners in hulls {
            let points: Vec<Vector> = corners.iter().map(|&p| metres(p)).collect();
            if let Some(collider) = ColliderBuilder::convex_hull(&points) {
                self.world.colliders.insert(collider.friction(DEFAULT_FRICTION));
                added += 1;
            }
        }
        if !triangles.is_empty()
            && let Ok(collider) = ColliderBuilder::trimesh(vertices.iter().map(|&p| metres(p)).collect(), triangles.to_vec())
        {
            self.world.colliders.insert(collider.friction(DEFAULT_FRICTION));
        }
        added
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
        let rigid = RigidBodyBuilder::dynamic()
            .pose(Pose::from_parts(metres(body.origin), rotation(body.angles)))
            .linear_damping(body.damping)
            .angular_damping(body.rot_damping)
            .user_data(u128::from(body.user));
        let handle = self.world.insert_body(rigid);
        for (shape, _) in shapes {
            let collider = ColliderBuilder::new(shape).density(density).friction(DEFAULT_FRICTION);
            self.world.insert_collider(collider, Some(handle));
        }
        if body.inertia > 0.0 && body.inertia != 1.0 {
            self.inertia.insert(handle, body.inertia);
        }
        self.weigh(handle);
        Some(BodyId(handle))
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
            self.world.insert_collider(collider.friction(DEFAULT_FRICTION), Some(handle));
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

    /// How fast moving and turning die away, per second.
    pub fn damping(&self, id: BodyId) -> (f32, f32) {
        self.world.bodies.get(id.0).map_or((0.0, 0.0), |b| (b.linear_damping(), b.angular_damping()))
    }

    pub fn set_damping(&mut self, id: BodyId, linear: f32, angular: f32) {
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.set_linear_damping(linear.max(0.0));
            body.set_angular_damping(angular.max(0.0));
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
                GenericJointBuilder::new(JointAxesMask::empty())
                    .coupled_axes(JointAxesMask::LIN_AXES)
                    .limits(JointAxis::LinX, [if rigid { length } else { 0.0 }, length])
                    .local_anchor1(first.inverse_transform_point(metres(start)))
                    .local_anchor2(second.inverse_transform_point(metres(end)))
                    .build()
            }
            Joint::Spring { start, end, length, stiffness, damping } => {
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
                let still = ranges.iter().all(|r| r[0].abs() < 1e-5 && r[1].abs() < 1e-5);
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
        self.limits.retain(|limit| limit.joint != id);
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

    /// The impacts of the step just made: what the contacts pushed apart, as the change of
    /// speed of each body that moves (the impulse over its mass).
    fn find_impacts(&mut self) {
        let world = &self.world;
        for pair in world.narrow_phase.contact_pairs() {
            if !pair.has_any_active_contact() {
                continue;
            }
            let impulse = pair.total_impulse_magnitude();
            if impulse <= 0.0 {
                continue;
            }
            let (_, normal) = pair.max_impulse();
            let point = pair
                .find_deepest_contact()
                .and_then(|(_, contact)| world.colliders.get(pair.collider1).map(|c| units(c.position() * contact.local_p1)));
            let body = |collider| world.colliders.get(collider).and_then(|c| c.parent());
            let (one, two) = (body(pair.collider1), body(pair.collider2));
            for (me, other, towards) in [(one, two, normal), (two, one, -normal)] {
                let Some(handle) = me else { continue };
                let Some(rb) = world.bodies.get(handle) else { continue };
                if !rb.is_dynamic() || rb.mass() <= 0.0 {
                    continue;
                }
                let speed = impulse / rb.mass() / METRES_PER_UNIT;
                if speed < MIN_IMPACT_SPEED || self.last_impact.get(&handle).is_some_and(|&t| self.clock - t < IMPACT_PAUSE) {
                    continue;
                }
                self.last_impact.insert(handle, self.clock);
                // the world's solids have no body; joints' ground is no thing to hit
                let other = other.filter(|&h| h != self.ground);
                self.impacts.push(Impact {
                    body: BodyId(handle),
                    user: rb.user_data as u64,
                    other_body: other.map(BodyId),
                    other: other.and_then(|h| world.bodies.get(h)).map(|b| b.user_data as u64),
                    speed,
                    point: point.unwrap_or_else(|| units(rb.translation())),
                    normal: towards.to_array(),
                });
            }
        }
        // bodies gone make no more impacts
        let bodies = &world.bodies;
        self.last_impact.retain(|handle, _| bodies.get(*handle).is_some());
    }

    /// Moves time on by `seconds`. Joints pulled harder than they take break.
    pub fn step(&mut self, seconds: f32) {
        if seconds <= 0.0 {
            return;
        }
        self.world.integration_parameters.dt = seconds;
        self.aim_limits();
        self.world.step_with_events(&Apart { pairs: &self.apart, aloof: &self.aloof }, &());
        self.clock += seconds;
        self.find_impacts();
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
}

impl PhysicsHooks for Apart<'_> {
    fn filter_contact_pair(&self, context: &PairFilterContext) -> Option<SolverFlags> {
        let (a, b) = (context.rigid_body1, context.rigid_body2);
        let aloof = |h: Option<RigidBodyHandle>| h.is_some_and(|h| self.aloof.contains(&h));
        let moves = |h: Option<RigidBodyHandle>| h.and_then(|h| context.bodies.get(h)).is_some_and(|b| b.is_dynamic());
        match (a, b) {
            (Some(a), Some(b)) if self.pairs.contains(&(a, b)) => None,
            _ if aloof(a) != aloof(b) && (aloof(a) && moves(b) || aloof(b) && moves(a)) => None,
            _ => Some(SolverFlags::COMPUTE_RIGID_IMPULSES),
        }
    }
}

#[cfg(test)]
mod tests;
