//! Physics of entities: their bodies in [`gmod_physics`], and the `PhysObj` of Lua.
//!
//! `PhysicsInit(SOLID_VPHYSICS)` gives an entity a body made of the first solid of its model's
//! physics model (`.phy`), with its mass and damping; `PhysicsInitBox`, `PhysicsInitSphere`
//! and `PhysicsInitConvex` give it one of a box, a ball or the hull of points. The engine's
//! props (`prop_physics` ...) get theirs when they spawn, frozen when the map says "motion
//! disabled". The world is the map's: its brushes, displacements and static props.
//!
//! The world entity's body is the ground of [`gmod_physics`]: it has no shape (the map's
//! solids are apart from it) and never moves, and constraints join bodies to it.
//!
//! Every tick the bodies move on, and the entities of those awake go where their bodies are;
//! constraints pulled too hard break ([`crate::constraint`]). Moving an entity (`SetPos`,
//! `SetAngles`) moves its body.

use std::cell::RefCell;
use std::collections::HashMap;

use gmod_physics::{Body, BodyId, Physics, Shape};
use mlua::{Lua, MultiValue, Value};

use crate::args::{self, Args};
use crate::entity::{self, EntityHandle, PhysObjHandle};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};
use crate::{host, timer};

/// The physics of a map: its world, and the bodies of entities.
#[derive(Default)]
pub(crate) struct World {
    pub physics: RefCell<Physics>,
    /// Whose each body is, and what Lua keeps with it.
    bodies: RefCell<HashMap<BodyId, Kept>>,
    /// The physics models read, by model.
    models: RefCell<HashMap<String, Option<PhysModel>>>,
}

/// What a physics model gives a body.
#[derive(Debug, Clone)]
struct PhysModel {
    shape: Shape,
    mass: Option<f32>,
    damping: f32,
    rot_damping: f32,
    material: String,
    /// How many solids it has: one for a prop, more for a ragdoll.
    solids: usize,
}

/// What Lua keeps with a body.
#[derive(Debug, Clone)]
struct Kept {
    entity: EntityHandle,
    material: String,
    /// Its bounds in its own space.
    mins: Vector,
    maxs: Vector,
    game_flags: i64,
    contents: i64,
    drag: bool,
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

fn vector(v: [f32; 3]) -> Vector {
    Vector::new(v[0], v[1], v[2])
}

fn turn(a: Angle) -> [f32; 3] {
    [a.p, a.y, a.r]
}

fn angle(a: [f32; 3]) -> Angle {
    Angle::new(a[0], a[1], a[2])
}

/// The bounds of a shape in its own space.
fn bounds(shape: &Shape) -> (Vector, Vector) {
    match shape {
        Shape::Hulls(hulls) => {
            let points = || hulls.iter().flatten();
            let lo = std::array::from_fn(|i| points().map(|p| p[i]).fold(f32::MAX, f32::min));
            let hi = std::array::from_fn(|i| points().map(|p| p[i]).fold(f32::MIN, f32::max));
            (vector(lo), vector(hi))
        }
        Shape::Sphere(r) => (Vector::new(-r, -r, -r), Vector::new(*r, *r, *r)),
    }
}

/// Makes the world of a map: what is solid in it. Bodies there were are gone.
pub(crate) fn load_world(lua: &Lua, collision: Option<&gmod_bsp::Collision>) {
    let mut physics = Physics::new();
    if let Some(collision) = collision {
        let shapes = collision.solid_shapes(gmod_bsp::mask::SOLID_BRUSHONLY);
        physics.add_world(&shapes.hulls, &shapes.vertices, &shapes.triangles);
    }
    {
        let host = host(lua);
        *host.physics.physics.borrow_mut() = physics;
        for (_, kept) in host.physics.bodies.borrow_mut().drain() {
            host.entities.with_mut(kept.entity, |ent| ent.physics = None);
        }
    }
    attach_world(lua);
}

/// Gives the world entity its body: the ground.
fn attach_world(lua: &Lua) {
    let host = host(lua);
    let world = EntityHandle::WORLD;
    if !host.entities.is_valid(world) {
        return;
    }
    let ground = host.physics.physics.borrow().ground();
    let kept = Kept {
        entity: world,
        material: "default".to_string(),
        mins: Vector::default(),
        maxs: Vector::default(),
        game_flags: 0,
        contents: 0,
        drag: false,
    };
    host.physics.bodies.borrow_mut().insert(ground, kept);
    host.entities.with_mut(world, |ent| ent.physics = Some(ground));
}

/// The physics model of a studio model, read once.
fn phys_model(lua: &Lua, model: &str) -> Option<PhysModel> {
    if model.is_empty() || model.starts_with('*') {
        return None;
    }
    let path = model.replace('\\', "/").to_ascii_lowercase();
    let host = host(lua);
    if let Some(known) = host.physics.models.borrow().get(&path) {
        return known.clone();
    }
    let read = || -> Option<PhysModel> {
        let mdl = gmod_model::Mdl::parse(&host.vfs.read(&path, "GAME").ok()?).ok()?;
        let phy = gmod_model::load_physics(&host.vfs, &path, mdl.checksum).ok()??;
        let solid = phy.solids.first()?;
        Some(PhysModel {
            shape: Shape::Hulls(solid.convexes.iter().map(|c| c.points.clone()).collect()),
            mass: solid.mass(),
            damping: solid.keys.number("damping").unwrap_or(0.0),
            rot_damping: solid.keys.number("rotdamping").unwrap_or(0.0),
            material: solid.surface_prop().unwrap_or("default").to_string(),
            solids: phy.solids.len(),
        })
    };
    let found = read();
    host.physics.models.borrow_mut().insert(path, found.clone());
    found
}

/// How many solids a model's physics model has: none without one, one for a prop, more for a
/// ragdoll.
pub(crate) fn solids(lua: &Lua, model: &str) -> usize {
    phys_model(lua, model).map_or(0, |m| m.solids)
}

/// The body of an entity, if it has one.
pub(crate) fn body(lua: &Lua, handle: EntityHandle) -> Option<BodyId> {
    host(lua).entities.get(handle)?.physics
}

/// Gives an entity a body of `shape` where it stands, in place of the one it had. Whether it
/// got one.
fn init(lua: &Lua, handle: EntityHandle, shape: Shape, model: Option<&PhysModel>) -> bool {
    destroy(lua, handle);
    let host = host(lua);
    let Some((pos, angles)) = host.entities.transform(handle) else { return false };
    let (mins, maxs) = bounds(&shape);
    let body = Body {
        mass: model.and_then(|m| m.mass),
        damping: model.map_or(0.0, |m| m.damping),
        rot_damping: model.map_or(0.0, |m| m.rot_damping),
        ..Body::new(shape, array(pos), turn(angles))
    };
    let Some(id) = host.physics.physics.borrow_mut().add(&body) else { return false };
    let material = model.map_or("default", |m| &m.material).to_string();
    let kept = Kept { entity: handle, material, mins, maxs, game_flags: 0, contents: 0, drag: true };
    host.physics.bodies.borrow_mut().insert(id, kept);
    host.entities.with_mut(handle, |ent| ent.physics = Some(id));
    true
}

/// `PhysicsInit(SOLID_VPHYSICS)`: a body of the entity's model. Whether it got one.
pub(crate) fn init_model(lua: &Lua, handle: EntityHandle) -> bool {
    let model = host(lua).entities.get(handle).map(|ent| ent.model.clone()).unwrap_or_default();
    match phys_model(lua, &model) {
        Some(found) => init(lua, handle, found.shape.clone(), Some(&found)),
        None => {
            destroy(lua, handle);
            false
        }
    }
}

/// A body of a box, a ball or convex hulls by their corners.
pub(crate) fn init_shape(lua: &Lua, handle: EntityHandle, shape: Shape) -> bool {
    init(lua, handle, shape, None)
}

/// Takes an entity's body away. The world keeps its own.
pub(crate) fn destroy(lua: &Lua, handle: EntityHandle) {
    if handle.index == Some(0) {
        return;
    }
    let host = host(lua);
    if let Some(id) = host.entities.with_mut(handle, |ent| ent.physics.take()).flatten() {
        host.physics.physics.borrow_mut().remove(id);
        host.physics.bodies.borrow_mut().remove(&id);
    }
}

/// An entity was moved: its body goes along.
pub(crate) fn moved(lua: &Lua, handle: EntityHandle) {
    let Some(id) = body(lua, handle) else { return };
    let host = host(lua);
    let Some((pos, angles)) = host.entities.transform(handle) else { return };
    host.physics.physics.borrow_mut().set_pose(id, array(pos), turn(angles));
}

/// Freezes a body or lets it move.
pub(crate) fn set_motion(lua: &Lua, handle: EntityHandle, moves: bool) {
    if let Some(id) = body(lua, handle) {
        host(lua).physics.physics.borrow_mut().set_motion(id, moves);
    }
}

pub(crate) fn sleep(lua: &Lua, handle: EntityHandle) {
    if let Some(id) = body(lua, handle) {
        host(lua).physics.physics.borrow_mut().sleep(id);
    }
}

pub(crate) fn wake(lua: &Lua, handle: EntityHandle) {
    if let Some(id) = body(lua, handle) {
        host(lua).physics.physics.borrow_mut().wake(id);
    }
}

/// One tick: the bodies move on, gravity as `sv_gravity` says, and the entities of those awake
/// go where their bodies are - unless they have a parent, which takes them along. Then the
/// constraints that broke go.
pub(crate) fn simulate(lua: &Lua) {
    let gravity = crate::convar::value(lua, "sv_gravity").and_then(|g| g.trim().parse::<f32>().ok()).unwrap_or(600.0);
    let broken = {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        physics.set_gravity(gravity);
        physics.step(timer::TICK_INTERVAL as f32);
        let bodies = host.physics.bodies.borrow();
        for id in physics.awake() {
            let Some(kept) = bodies.get(&id) else { continue };
            let Some((origin, angles)) = physics.pose(id) else { continue };
            let velocity = vector(physics.velocity(id));
            host.entities.with_mut(kept.entity, |ent| {
                if ent.parent.index.is_none() {
                    ent.local_pos = vector(origin);
                    ent.local_angles = angle(angles);
                }
                ent.velocity = velocity;
            });
        }
        physics.broken()
    };
    crate::constraint::broken(lua, &broken);
}

/// The `PhysObj` of an entity: valid while the entity has a body.
pub(crate) fn push(lua: &Lua, handle: EntityHandle) -> mlua::Result<Value> {
    let owner = if body(lua, handle).is_some() { handle } else { EntityHandle::NULL };
    Ok(Value::UserData(lua.create_userdata(PhysObjHandle(owner))?))
}

/// Whether a `PhysObj` is valid.
pub(crate) fn valid(lua: &Lua, phys: PhysObjHandle) -> bool {
    host(lua).entities.is_valid(phys.0) && body(lua, phys.0).is_some()
}

/// The entity and body of the `PhysObj` in argument 1, which must be valid.
fn this(a: &Args) -> mlua::Result<(EntityHandle, BodyId)> {
    let phys = a.userdata::<PhysObjHandle>(1, "PhysObj")?;
    match body(a.lua, phys.0).filter(|_| host(a.lua).entities.is_valid(phys.0)) {
        Some(id) => Ok((phys.0, id)),
        None => Err(args::error(a.lua, "Tried to use a NULL physics object!")),
    }
}

fn with<R>(a: &Args, f: impl FnOnce(&mut Physics, BodyId) -> R) -> mlua::Result<R> {
    let (_, id) = this(a)?;
    let host = host(a.lua);
    let mut physics = host.physics.physics.borrow_mut();
    Ok(f(&mut physics, id))
}

fn kept<R>(a: &Args, f: impl FnOnce(&mut Kept) -> R) -> mlua::Result<R> {
    let (_, id) = this(a)?;
    let host = host(a.lua);
    let mut bodies = host.physics.bodies.borrow_mut();
    Ok(f(bodies.get_mut(&id).expect("every body is kept")))
}

fn vector_value(lua: &Lua, v: Vector) -> mlua::Result<Value> {
    entity::vector(lua, v)
}

/// Where a body is, as an entity's position and angles.
fn pose(a: &Args) -> mlua::Result<(Vector, Angle)> {
    let (origin, angles) = with(a, |p, id| p.pose(id))?.unwrap_or_default();
    Ok((vector(origin), angle(angles)))
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    attach_world(lua);
    let phys = meta::meta_table(lua, "PhysObj", type_id::PHYSOBJ)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        phys.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("IsValid", |a| Ok(Value::Boolean(a.userdata::<PhysObjHandle>(1, "PhysObj").is_ok_and(|p| valid(a.lua, p)))))?;
    def("GetEntity", |a| entity::push(a.lua, this(a)?.0))?;
    def("Wake", |a| with(a, |p, id| p.wake(id)).map(|_| Value::Nil))?;
    def("Sleep", |a| with(a, |p, id| p.sleep(id)).map(|_| Value::Nil))?;
    def("IsAsleep", |a| Ok(Value::Boolean(with(a, |p, id| p.asleep(id))?)))?;
    def("EnableMotion", |a| {
        let on = a.boolean(2);
        with(a, |p, id| p.set_motion(id, on)).map(|_| Value::Nil)
    })?;
    def("IsMotionEnabled", |a| Ok(Value::Boolean(with(a, |p, id| p.motion(id))?)))?;
    def("IsMoveable", |a| Ok(Value::Boolean(with(a, |p, id| p.motion(id))?)))?;
    def("EnableGravity", |a| {
        let on = a.boolean(2);
        with(a, |p, id| p.set_gravity_enabled(id, on)).map(|_| Value::Nil)
    })?;
    def("IsGravityEnabled", |a| Ok(Value::Boolean(with(a, |p, id| p.gravity_enabled(id))?)))?;
    def("EnableCollisions", |a| {
        let on = a.boolean(2);
        with(a, |p, id| p.set_collisions(id, on)).map(|_| Value::Nil)
    })?;
    def("IsCollisionEnabled", |a| Ok(Value::Boolean(with(a, |p, id| p.collisions(id))?)))?;
    def("EnableDrag", |a| {
        let on = a.boolean(2);
        kept(a, |k| k.drag = on).map(|_| Value::Nil)
    })?;
    def("IsDragEnabled", |a| Ok(Value::Boolean(kept(a, |k| k.drag)?)))?;
    // drag and buoyancy are not simulated yet
    def("SetDragCoefficient", |a| this(a).map(|_| Value::Nil))?;
    def("SetAngleDragCoefficient", |a| this(a).map(|_| Value::Nil))?;
    def("SetBuoyancyRatio", |a| this(a).map(|_| Value::Nil))?;
    def("RecheckCollisionFilter", |a| this(a).map(|_| Value::Nil))?;
    def("SetInertia", |a| this(a).map(|_| Value::Nil))?;
    def("IsPenetrating", |a| this(a).map(|_| Value::Boolean(false)))?;
    def("GetMass", |a| Ok(Value::Number(f64::from(with(a, |p, id| p.mass(id))?))))?;
    def("GetInvMass", |a| {
        let mass = with(a, |p, id| p.mass(id))?;
        Ok(Value::Number(if mass > 0.0 { 1.0 / f64::from(mass) } else { 0.0 }))
    })?;
    def("SetMass", |a| {
        let mass = a.number(2)? as f32;
        with(a, |p, id| p.set_mass(id, mass.max(0.001))).map(|_| Value::Nil)
    })?;
    def("GetMassCenter", |a| vector_value(a.lua, vector(with(a, |p, id| p.mass_center(id))?)))?;
    def("GetVolume", |a| Ok(Value::Number(f64::from(with(a, |p, id| p.volume(id))?))))?;
    def("GetPos", |a| vector_value(a.lua, pose(a)?.0))?;
    def("GetAngles", |a| entity::angle(a.lua, pose(a)?.1))?;
    def("SetPos", |a| {
        let (handle, _) = this(a)?;
        let pos = a.userdata::<Vector>(2, "Vector")?;
        entity::set_world_pos(a.lua, handle, pos);
        Ok(Value::Nil)
    })?;
    def("SetAngles", |a| {
        let (handle, _) = this(a)?;
        let angles = a.userdata::<Angle>(2, "Angle")?;
        entity::set_world_angles(a.lua, handle, angles);
        Ok(Value::Nil)
    })?;
    def("GetVelocity", |a| vector_value(a.lua, vector(with(a, |p, id| p.velocity(id))?)))?;
    let set_velocity = |a: &Args| -> mlua::Result<Value> {
        let v = array(a.userdata::<Vector>(2, "Vector")?);
        with(a, |p, id| p.set_velocity(id, v)).map(|_| Value::Nil)
    };
    def("SetVelocity", set_velocity)?;
    def("SetVelocityInstantaneous", set_velocity)?;
    def("AddVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        with(a, |p, id| {
            let now = vector(p.velocity(id));
            p.set_velocity(id, array(now + v));
        })
        .map(|_| Value::Nil)
    })?;
    def("GetVelocityAtPoint", |a| {
        let point = array(a.userdata::<Vector>(2, "Vector")?);
        vector_value(a.lua, vector(with(a, |p, id| p.velocity_at(id, point))?))
    })?;
    def("GetAngleVelocity", |a| vector_value(a.lua, vector(with(a, |p, id| p.angular_velocity(id))?)))?;
    let set_angle_velocity = |a: &Args| -> mlua::Result<Value> {
        let v = array(a.userdata::<Vector>(2, "Vector")?);
        with(a, |p, id| p.set_angular_velocity(id, v)).map(|_| Value::Nil)
    };
    def("SetAngleVelocity", set_angle_velocity)?;
    def("SetAngleVelocityInstantaneous", set_angle_velocity)?;
    def("AddAngleVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        with(a, |p, id| {
            let now = vector(p.angular_velocity(id));
            p.set_angular_velocity(id, array(now + v));
        })
        .map(|_| Value::Nil)
    })?;
    def("ApplyForceCenter", |a| {
        let force = array(a.userdata::<Vector>(2, "Vector")?);
        with(a, |p, id| p.apply_impulse(id, force)).map(|_| Value::Nil)
    })?;
    def("ApplyForceOffset", |a| {
        let force = array(a.userdata::<Vector>(2, "Vector")?);
        let point = array(a.userdata::<Vector>(3, "Vector")?);
        with(a, |p, id| p.apply_impulse_at(id, force, point)).map(|_| Value::Nil)
    })?;
    def("ApplyTorqueCenter", |a| {
        let torque = array(a.userdata::<Vector>(2, "Vector")?);
        with(a, |p, id| p.apply_torque_impulse(id, torque)).map(|_| Value::Nil)
    })?;
    // the methods that give two values
    phys.raw_set("GetDamping", lua.create_function(damping)?)?;
    phys.raw_set("GetAABB", lua.create_function(aabb)?)?;
    def("GetSpeedDamping", |a| Ok(Value::Number(f64::from(with(a, |p, id| p.damping(id).0)?))))?;
    def("GetRotDamping", |a| Ok(Value::Number(f64::from(with(a, |p, id| p.damping(id).1)?))))?;
    def("SetDamping", |a| {
        let (linear, angular) = (a.number(2)? as f32, a.number(3)? as f32);
        with(a, |p, id| p.set_damping(id, linear, angular)).map(|_| Value::Nil)
    })?;
    def("LocalToWorld", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        let (pos, angles) = pose(a)?;
        vector_value(a.lua, pos + v.rotated(angles))
    })?;
    def("WorldToLocal", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        let (pos, angles) = pose(a)?;
        vector_value(a.lua, entity::unrotated(v - pos, angles))
    })?;
    // about an axis of the world through its origin: the angles it would have
    def("RotateAroundAxis", |a| {
        let (axis, degrees) = (a.userdata::<Vector>(2, "Vector")?, a.number(3)? as f32);
        entity::angle(a.lua, pose(a)?.1.rotated_about(axis, degrees))
    })?;
    def("AlignAngles", |a| {
        let (from, to) = (a.userdata::<Angle>(2, "Angle")?, a.userdata::<Angle>(3, "Angle")?);
        entity::angle(a.lua, entity::align(pose(a)?.1, from, to))
    })?;
    def("LocalToWorldVector", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        vector_value(a.lua, v.rotated(pose(a)?.1))
    })?;
    def("WorldToLocalVector", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        vector_value(a.lua, entity::unrotated(v, pose(a)?.1))
    })?;
    def("GetMaterial", |a| Ok(Value::String(a.lua.create_string(kept(a, |k| k.material.clone())?)?)))?;
    def("SetMaterial", |a| {
        let material = a.string(2)?;
        kept(a, |k| k.material = material).map(|_| Value::Nil)
    })?;
    def("GetContents", |a| Ok(Value::Integer(kept(a, |k| k.contents)?)))?;
    def("SetContents", |a| {
        let contents = a.number(2)? as i64;
        kept(a, |k| k.contents = contents).map(|_| Value::Nil)
    })?;
    def("AddGameFlag", |a| {
        let flags = a.number(2)? as i64;
        kept(a, |k| k.game_flags |= flags).map(|_| Value::Nil)
    })?;
    def("ClearGameFlag", |a| {
        let flags = a.number(2)? as i64;
        kept(a, |k| k.game_flags &= !flags).map(|_| Value::Nil)
    })?;
    def("HasGameFlag", |a| {
        let flags = a.number(2)? as i64;
        Ok(Value::Boolean(kept(a, |k| k.game_flags & flags != 0)?))
    })?;
    def("GetStress", |a| this(a).map(|_| Value::Number(0.0)))?;
    def("GetFrictionSnapshot", |a| this(a).and_then(|_| a.lua.create_table().map(Value::Table)))?;
    def("OutputDebugInfo", |a| {
        let (handle, _) = this(a)?;
        let mass = with(a, |p, id| p.mass(id))?;
        let (pos, _) = pose(a)?;
        host(a.lua).print(&format!("PhysObj of entity {:?}: mass {mass}, at {} {} {}\n", handle.index, pos.x, pos.y, pos.z));
        Ok(Value::Nil)
    })?;
    Ok(())
}

/// `PhysObj:GetAABB()`: the body's bounds in its own space, two values.
fn aabb(lua: &Lua, values: MultiValue) -> mlua::Result<MultiValue> {
    let a = Args::new(lua, values, "GetAABB");
    let (mins, maxs) = kept(&a, |k| (k.mins, k.maxs))?;
    Ok(MultiValue::from_iter([entity::vector(lua, mins)?, entity::vector(lua, maxs)?]))
}

/// `PhysObj:GetDamping()`: linear and angular, two values.
fn damping(lua: &Lua, values: MultiValue) -> mlua::Result<MultiValue> {
    let a = Args::new(lua, values, "GetDamping");
    let (linear, angular) = with(&a, |p, id| p.damping(id))?;
    Ok(MultiValue::from_iter([Value::Number(f64::from(linear)), Value::Number(f64::from(angular))]))
}
