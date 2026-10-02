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

#![forbid(unsafe_code)]

use rapier3d::math::{Mat3, Pose, Rotation, Vector};
use rapier3d::prelude::{
    ColliderBuilder, ColliderHandle, InteractionGroups, PhysicsWorld, QueryFilter, Ray, RigidBodyBuilder, RigidBodyHandle, RigidBodyType,
    SharedShape,
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
    /// Whatever the caller keeps with it: its entity.
    pub user: u64,
}

impl Body {
    pub fn new(shape: Shape, origin: Vec3, angles: Vec3) -> Body {
        Body { shape, mass: None, origin, angles, damping: 0.0, rot_damping: 0.0, user: 0 }
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

/// The world and the bodies in it.
pub struct Physics {
    world: PhysicsWorld,
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
        Physics { world }
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
        Some(BodyId(handle))
    }

    pub fn remove(&mut self, id: BodyId) {
        self.world.remove_body(id.0);
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

    /// Moves a body there at once, waking it.
    pub fn set_pose(&mut self, id: BodyId, origin: Vec3, angles: Vec3) {
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

    pub fn set_motion(&mut self, id: BodyId, moves: bool) {
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
        if let Some(body) = self.world.bodies.get_mut(id.0) {
            body.recompute_mass_properties_from_colliders(&self.world.colliders);
            body.wake_up(true);
        }
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

    /// The box around a body in the world.
    pub fn bounds(&self, id: BodyId) -> Option<(Vec3, Vec3)> {
        let mut found: Option<(Vector, Vector)> = None;
        for handle in self.colliders(id) {
            let Some(collider) = self.world.colliders.get(handle) else { continue };
            let aabb = collider.compute_aabb();
            found = Some(found.map_or((aabb.mins, aabb.maxs), |(lo, hi)| (lo.min(aabb.mins), hi.max(aabb.maxs))));
        }
        found.map(|(lo, hi)| (units(lo), units(hi)))
    }

    /// The first body a ray from `start` to `end` hits, past `skip`; the world is not looked at.
    /// Bodies are found where the last step left them.
    pub fn ray(&self, start: Vec3, end: Vec3, skip: Option<BodyId>) -> Option<Hit> {
        let (from, to) = (metres(start), metres(end));
        let length = (to - from).length();
        if length <= 0.0 {
            return None;
        }
        let ray = Ray::new(from, (to - from) / length);
        let bodies_only = |_: ColliderHandle, c: &rapier3d::prelude::Collider| c.parent().is_some();
        let mut filter = QueryFilter::new().predicate(&bodies_only);
        if let Some(skip) = skip {
            filter = filter.exclude_rigid_body(skip.0);
        }
        let (collider, hit) = self.world.cast_ray_and_get_normal(&ray, length, true, filter)?;
        let body = self.world.colliders.get(collider)?.parent()?;
        Some(Hit { body: BodyId(body), fraction: hit.time_of_impact / length, normal: hit.normal.to_array() })
    }

    /// The bodies that move and are awake.
    pub fn awake(&self) -> Vec<BodyId> {
        self.world.active_bodies().map(|(handle, _)| BodyId(handle)).collect()
    }

    /// Moves time on by `seconds`.
    pub fn step(&mut self, seconds: f32) {
        if seconds <= 0.0 {
            return;
        }
        self.world.integration_parameters.dt = seconds;
        self.world.step();
    }
}

#[cfg(test)]
mod tests;
