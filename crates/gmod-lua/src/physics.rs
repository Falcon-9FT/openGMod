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
//!
//! A ragdoll has a body for each solid of its model ([`crate::ragdoll`]): its `PhysObj`s by
//! number, the first its entity's, which follows it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gmod_physics::{Body, BodyId, Physics, Shape};
use mlua::{Lua, MultiValue, ObjectLike, Table, Value};

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
    /// The ragdolls, by the number of their entity.
    pub ragdolls: RefCell<HashMap<u32, crate::ragdoll::Ragdoll>>,
}

/// What a physics model gives a body.
#[derive(Debug, Clone)]
struct PhysModel {
    shape: Shape,
    mass: Option<f32>,
    damping: f32,
    rot_damping: f32,
    material: String,
    /// All of it: the solids of a ragdoll, how they hang together.
    phy: Arc<gmod_model::Phy>,
    /// Its shape for traces: the convex hulls of its first solid.
    hulls: Arc<Vec<gmod_bsp::collision::Hull>>,
}

/// What Lua keeps with a body.
#[derive(Debug, Clone)]
struct Kept {
    entity: EntityHandle,
    /// Which of its entity's: 0, or a part of a ragdoll.
    part: usize,
    material: String,
    /// Its bounds in its own space.
    mins: Vector,
    maxs: Vector,
    game_flags: i64,
    contents: i64,
    drag: bool,
    /// The shape of its model, which traces sweep through; none for a box, a ball or hulls
    /// Lua made.
    hulls: Option<Arc<Vec<gmod_bsp::collision::Hull>>>,
}

pub(crate) fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

pub(crate) fn vector(v: [f32; 3]) -> Vector {
    Vector::new(v[0], v[1], v[2])
}

pub(crate) fn turn(a: Angle) -> [f32; 3] {
    [a.p, a.y, a.r]
}

pub(crate) fn angle(a: [f32; 3]) -> Angle {
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
        // their bodies went with the old world, whose handles the new one gives out again
        host.physics.ragdolls.borrow_mut().clear();
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
        part: 0,
        material: "default".to_string(),
        mins: Vector::default(),
        maxs: Vector::default(),
        game_flags: 0,
        contents: 0,
        drag: false,
        hulls: None,
    };
    host.physics.bodies.borrow_mut().insert(ground, kept);
    host.entities.with_mut(world, |ent| ent.physics = Some(ground));
}

/// The models that had no physics model are looked for again (an addon was mounted).
pub(crate) fn forget_missing(lua: &Lua) {
    host(lua).physics.models.borrow_mut().retain(|_, model| model.is_some());
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
        let phy = Arc::new(gmod_model::load_physics(&host.vfs, &path, mdl.checksum).ok()??);
        let solid = phy.solids.first()?;
        let hulls = solid
            .convexes
            .iter()
            .filter_map(|c| gmod_bsp::collision::Hull::new(&c.points, &c.triangles.iter().map(|t| t.map(usize::from)).collect::<Vec<_>>()))
            .collect();
        Some(PhysModel {
            shape: Shape::Hulls(solid.convexes.iter().map(|c| c.points.clone()).collect()),
            mass: solid.mass(),
            damping: solid.keys.number("damping").unwrap_or(0.0),
            rot_damping: solid.keys.number("rotdamping").unwrap_or(0.0),
            material: solid.surface_prop().unwrap_or("default").to_string(),
            phy: phy.clone(),
            hulls: Arc::new(hulls),
        })
    };
    let found = read();
    host.physics.models.borrow_mut().insert(path, found.clone());
    found
}

/// How many solids a model's physics model has: none without one, one for a prop, more for a
/// ragdoll.
pub(crate) fn solids(lua: &Lua, model: &str) -> usize {
    phys_model(lua, model).map_or(0, |m| m.phy.solids.len())
}

/// A model's physics model, read once.
pub(crate) fn phy(lua: &Lua, model: &str) -> Option<Arc<gmod_model::Phy>> {
    phys_model(lua, model).map(|m| m.phy)
}

/// Keeps a body of an entity: what Lua keeps with it, the bounds of its shape in its own space.
pub(crate) fn keep(lua: &Lua, id: BodyId, entity: EntityHandle, part: usize, shape: &Shape, material: &str) {
    let (mins, maxs) = bounds(shape);
    let kept = Kept { entity, part, material: material.to_string(), mins, maxs, game_flags: 0, contents: 0, drag: true, hulls: None };
    host(lua).physics.bodies.borrow_mut().insert(id, kept);
    set_material(lua, id, material);
}

/// Takes a body away, with what Lua keeps with it.
pub(crate) fn remove_body(lua: &Lua, id: BodyId) {
    host(lua).physics.physics.borrow_mut().remove(id);
    host(lua).physics.bodies.borrow_mut().remove(&id);
}

/// The body of a `PhysObj`: of a part of a ragdoll, or an entity's own (part 0).
pub(crate) fn part_body(lua: &Lua, handle: EntityHandle, part: usize) -> Option<BodyId> {
    match crate::ragdoll::part_body(lua, handle, part) {
        Some(found) => found,
        None if part == 0 => body(lua, handle),
        None => None,
    }
}

/// Every body of an entity: its own, or each of a ragdoll's.
fn bodies(lua: &Lua, handle: EntityHandle) -> Vec<BodyId> {
    crate::ragdoll::bodies(lua, handle).unwrap_or_else(|| body(lua, handle).into_iter().collect())
}

/// How many `PhysObj`s an entity has (`GetPhysicsObjectCount`).
pub(crate) fn count(lua: &Lua, handle: EntityHandle) -> usize {
    crate::ragdoll::count(lua, handle).unwrap_or_else(|| usize::from(body(lua, handle).is_some()))
}

/// The body of an entity, if it has one.
pub(crate) fn body(lua: &Lua, handle: EntityHandle) -> Option<BodyId> {
    host(lua).entities.get(handle)?.physics
}

/// Gives an entity a body of `shape` where it stands, in place of the one it had. Whether it
/// got one.
fn init(lua: &Lua, handle: EntityHandle, shape: Shape, model: Option<&PhysModel>, material: Option<&str>) -> bool {
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
    let material = material.or(model.map(|m| m.material.as_str())).unwrap_or("default").to_string();
    let hulls = model.map(|m| m.hulls.clone()).filter(|h| !h.is_empty());
    let kept = Kept { entity: handle, part: 0, material: material.clone(), mins, maxs, game_flags: 0, contents: 0, drag: true, hulls };
    host.physics.bodies.borrow_mut().insert(id, kept);
    host.entities.with_mut(handle, |ent| ent.physics = Some(id));
    set_material(lua, id, &material);
    true
}

/// The shape of an entity's model that its body has, for traces: its convex hulls in its own
/// space, and their bounds. None for an entity without a body, or with a body of another shape.
pub(crate) fn shape(lua: &Lua, handle: EntityHandle) -> Option<(Arc<Vec<gmod_bsp::collision::Hull>>, Vector, Vector)> {
    let id = body(lua, handle)?;
    let host = host(lua);
    let bodies = host.physics.bodies.borrow();
    let kept = bodies.get(&id)?;
    Some((kept.hulls.clone()?, kept.mins, kept.maxs))
}

/// The shape of an entity's model for traces when it has no body of it - a client's copy of
/// the server's entity, one whose body was taken away - but is solid by it
/// (`SOLID_VPHYSICS`), as the engine traces the model's collision model where it stands.
pub(crate) fn model_shape(lua: &Lua, handle: EntityHandle) -> Option<(Arc<Vec<gmod_bsp::collision::Hull>>, Vector, Vector)> {
    let model = host(lua).entities.get(handle).filter(|ent| ent.solid == crate::entity::SOLID_VPHYSICS).map(|ent| ent.model.clone())?;
    let found = phys_model(lua, &model).filter(|m| !m.hulls.is_empty())?;
    let (mins, maxs) = bounds(&found.shape);
    Some((found.hulls, mins, maxs))
}

/// `PhysicsInit(SOLID_VPHYSICS)`: a body of the entity's model. Whether it got one.
pub(crate) fn init_model(lua: &Lua, handle: EntityHandle) -> bool {
    let model = host(lua).entities.get(handle).map(|ent| ent.model.clone()).unwrap_or_default();
    match phys_model(lua, &model) {
        Some(found) => init(lua, handle, found.shape.clone(), Some(&found), None),
        None => {
            destroy(lua, handle);
            false
        }
    }
}

/// A body of a box, a ball or convex hulls by their corners, of a surface property (`default`
/// when none is given).
pub(crate) fn init_shape(lua: &Lua, handle: EntityHandle, shape: Shape, material: Option<&str>) -> bool {
    init(lua, handle, shape, None, material)
}

/// A body is of a surface property from now on (`PhysObj:SetMaterial`): it holds back what
/// slides along it as that does (`friction_00` not at all).
pub(crate) fn set_material(lua: &Lua, id: BodyId, material: &str) {
    let friction = crate::surfaceprops::with(lua, |props| props.get(material).map(|p| p.friction));
    let host = host(lua);
    if let Some(kept) = host.physics.bodies.borrow_mut().get_mut(&id) {
        kept.material = material.to_string();
    }
    host.physics.physics.borrow_mut().set_friction(id, friction.unwrap_or(gmod_physics::DEFAULT_FRICTION));
}

/// Takes an entity's bodies away: its own, a ragdoll's parts. The world keeps its own.
pub(crate) fn destroy(lua: &Lua, handle: EntityHandle) {
    if handle.index == Some(0) {
        return;
    }
    let all = bodies(lua, handle);
    crate::ragdoll::forget(lua, handle);
    let host = host(lua);
    host.entities.with_mut(handle, |ent| ent.physics = None);
    for id in all {
        remove_body(lua, id);
    }
}

/// An entity was moved: its body goes along, a ragdoll's all of them.
pub(crate) fn moved(lua: &Lua, handle: EntityHandle) {
    if crate::ragdoll::moved(lua, handle) {
        return;
    }
    let Some(id) = body(lua, handle) else { return };
    let host = host(lua);
    let Some((pos, angles)) = host.entities.transform(handle) else { return };
    host.physics.physics.borrow_mut().set_pose(id, array(pos), turn(angles));
}

/// Freezes an entity's bodies or lets them move.
pub(crate) fn set_motion(lua: &Lua, handle: EntityHandle, moves: bool) {
    for id in bodies(lua, handle) {
        host(lua).physics.physics.borrow_mut().set_motion(id, moves);
    }
}

pub(crate) fn sleep(lua: &Lua, handle: EntityHandle) {
    for id in bodies(lua, handle) {
        host(lua).physics.physics.borrow_mut().sleep(id);
    }
}

pub(crate) fn wake(lua: &Lua, handle: EntityHandle) {
    for id in bodies(lua, handle) {
        host(lua).physics.physics.borrow_mut().wake(id);
    }
}

/// A body hit something: what Lua hears of it.
struct Collision {
    entity: EntityHandle,
    part: usize,
    material: String,
    /// What it hit: the world, or another entity with a body, and which of its bodies.
    other: EntityHandle,
    other_part: usize,
    other_material: Option<String>,
    speed: f32,
    point: Vector,
    normal: Vector,
    velocity: Vector,
    other_velocity: Vector,
}

/// One tick: the bodies move on, gravity as `sv_gravity` says, and the entities of those awake
/// go where their bodies are - unless they have a parent, which takes them along. Then the
/// constraints that broke go, and the bodies that hit something make their sounds and tell
/// Lua.
pub(crate) fn simulate(lua: &Lua) {
    let gravity = crate::convar::value(lua, "sv_gravity").and_then(|g| g.trim().parse::<f32>().ok()).unwrap_or(600.0);
    motion_controllers(lua);
    let (broken, collisions) = {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        physics.set_gravity(gravity);
        physics.step(timer::TICK_INTERVAL as f32);
        let bodies = host.physics.bodies.borrow();
        for id in physics.awake() {
            // a ragdoll's entity follows its first part
            let Some(kept) = bodies.get(&id).filter(|k| k.part == 0) else { continue };
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
        let collisions: Vec<Collision> = physics
            .impacts()
            .into_iter()
            .filter_map(|impact| {
                let kept = bodies.get(&impact.body)?;
                let other = impact.other_body.and_then(|id| bodies.get(&id).map(|k| (id, k)));
                Some(Collision {
                    entity: kept.entity,
                    part: kept.part,
                    material: kept.material.clone(),
                    other: other.map_or(EntityHandle::WORLD, |(_, k)| k.entity),
                    other_part: other.map_or(0, |(_, k)| k.part),
                    other_material: other.map(|(_, k)| k.material.clone()),
                    speed: impact.speed,
                    point: vector(impact.point),
                    normal: vector(impact.normal),
                    velocity: vector(physics.velocity(impact.body)),
                    other_velocity: other.map_or(Vector::default(), |(id, _)| vector(physics.velocity(id))),
                })
            })
            .collect();
        (physics.broken(), collisions)
    };
    crate::ragdoll::moved_on(lua);
    physics_updates(lua);
    crate::constraint::broken(lua, &broken);
    for collision in &collisions {
        if host(lua).entities.is_valid(collision.entity) {
            collided(lua, collision);
        }
    }
}

// how `PhysicsSimulate` says to push: nothing, or accelerations or forces about the body's own
// axes or the world's
const SIM_LOCAL_ACCELERATION: i64 = 1;
const SIM_LOCAL_FORCE: i64 = 2;
// SIM_GLOBAL_ACCELERATION is 3: neither local nor a force
const SIM_GLOBAL_FORCE: i64 = 4;

/// A vector about the axes of something turned by `angles`, about the world's.
fn to_world(angles: [f32; 3], v: Vector) -> Vector {
    let [f, l, u] = gmod_physics::axes(angles);
    Vector::new(f[0] * v.x + l[0] * v.y + u[0] * v.z, f[1] * v.x + l[1] * v.y + u[1] * v.z, f[2] * v.x + l[2] * v.y + u[2] * v.z)
}

/// A vector about the world's axes, about those of something turned by `angles`.
fn to_local(angles: [f32; 3], v: Vector) -> Vector {
    let [f, l, u] = gmod_physics::axes(angles);
    let dot = |a: [f32; 3]| a[0] * v.x + a[1] * v.y + a[2] * v.z;
    Vector::new(dot(f), dot(l), dot(u))
}

/// The motion controllers, before the bodies move: each entity that started one
/// (`StartMotionController`) has its `PhysicsSimulate(phys, dt)` asked, for each of its bodies
/// awake and those added to it, how to push it - an angular and a linear push, and whether
/// they are accelerations or forces, about the body's axes or the world's (`SIM_`) - and it is
/// pushed so for the tick.
fn motion_controllers(lua: &Lua) {
    let controllers: Vec<(EntityHandle, Vec<PhysObjHandle>)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                if !ent.motion_controller || ent.removed {
                    return None;
                }
                let mut objects = ent.motion_objects.clone();
                if !objects.iter().any(|p| p.0 == h) {
                    objects.insert(0, PhysObjHandle(h, 0));
                }
                Some((h, objects))
            })
            .collect()
    };
    let dt = timer::TICK_INTERVAL as f32;
    for (handle, objects) in controllers {
        let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { continue };
        let Ok(Value::Function(simulate)) = ud.get::<Value>("PhysicsSimulate") else { continue };
        for phys in objects {
            if !valid(lua, phys) {
                continue;
            }
            let Some(id) = part_body(lua, phys.0, phys.1) else { continue };
            let awake = {
                let host = host(lua);
                let physics = host.physics.physics.borrow();
                physics.motion(id) && !physics.asleep(id)
            };
            if !awake {
                continue;
            }
            let Ok(phys_value) = lua.create_userdata(phys) else { continue };
            let args = MultiValue::from_iter([Value::UserData(ud.clone()), Value::UserData(phys_value), Value::Number(f64::from(dt))]);
            let Ok(result) = crate::files::call_reporting(lua, simulate.clone(), args) else { continue };
            let mut result = result.into_iter();
            let (angular, linear, mode) = (result.next(), result.next(), result.next());
            let vector_of = |v: Option<Value>| match v {
                Some(Value::UserData(ud)) => ud.borrow::<Vector>().map(|v| *v).ok(),
                _ => None,
            };
            let mode = match mode {
                Some(Value::Integer(n)) => n,
                Some(Value::Number(n)) => n as i64,
                _ => 0,
            };
            let (angular, linear) = (vector_of(angular).unwrap_or_default(), vector_of(linear).unwrap_or_default());
            if !(SIM_LOCAL_ACCELERATION..=SIM_GLOBAL_FORCE).contains(&mode) {
                continue;
            }
            let host = host(lua);
            let mut physics = host.physics.physics.borrow_mut();
            let Some((_, angles)) = physics.pose(id) else { continue };
            // a force moves a body less the more it weighs
            let per = if mode == SIM_LOCAL_FORCE || mode == SIM_GLOBAL_FORCE { 1.0 / physics.mass(id).max(1e-3) } else { 1.0 };
            let local = mode == SIM_LOCAL_ACCELERATION || mode == SIM_LOCAL_FORCE;
            let linear = if local { to_world(angles, linear) } else { linear };
            let angular = if local { angular } else { to_local(angles, angular) };
            let velocity = vector(physics.velocity(id)) + linear * (per * dt);
            physics.set_velocity(id, array(velocity));
            let spin = vector(physics.angular_velocity(id)) + angular * (per * dt);
            physics.set_angular_velocity(id, array(spin));
        }
    }
}

/// After the bodies moved: the scripted entities whose bodies are awake hear of it
/// (`ENT:PhysicsUpdate(phys)`).
fn physics_updates(lua: &Lua) {
    let awake: Vec<EntityHandle> = {
        let host = host(lua);
        let physics = host.physics.physics.borrow();
        let bodies = host.physics.bodies.borrow();
        let mut list: Vec<EntityHandle> = physics
            .awake()
            .into_iter()
            .filter_map(|id| bodies.get(&id).filter(|k| k.part == 0).map(|k| k.entity))
            .filter(|&h| host.entities.get(h).is_some_and(|e| e.scripted && !e.removed))
            .collect();
        list.dedup();
        list
    };
    for handle in awake {
        let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { continue };
        if let Ok(Value::Function(update)) = ud.get::<Value>("PhysicsUpdate")
            && let Ok(phys) = push(lua, handle)
        {
            let _ = crate::files::call_reporting(lua, update, MultiValue::from_iter([Value::UserData(ud), phys]));
        }
    }
}

/// An impact: the body's surface sounds it, as Source does - from 70 units/s, as loud as the
/// square of the speed up to 320, hard from 300 - and Lua hears of it: `ENT:PhysicsCollide`
/// and the entity's `PhysicsCollide` callbacks.
fn collided(lua: &Lua, c: &Collision) {
    if c.speed >= 70.0 {
        let (_, surface) = crate::surfaceprops::named(lua, &c.material);
        let name = if c.speed >= 300.0 { &surface.impact_hard } else { &surface.impact_soft };
        let volume = (c.speed * c.speed / (320.0 * 320.0)).min(1.0);
        crate::sound::emit_scaled(lua, name, c.entity, volume);
    }
    let Ok(Value::UserData(ud)) = entity::push(lua, c.entity) else { return };
    let data = (|| -> mlua::Result<Table> {
        let t = lua.create_table()?;
        let (our, _) = crate::surfaceprops::named(lua, &c.material);
        let (their, _) = crate::surfaceprops::named(lua, c.other_material.as_deref().unwrap_or("default"));
        t.raw_set("HitPos", entity::vector(lua, c.point)?)?;
        t.raw_set("HitEntity", entity::push(lua, c.other)?)?;
        t.raw_set("HitNormal", entity::vector(lua, c.normal)?)?;
        t.raw_set("Speed", c.speed)?;
        t.raw_set("HitSpeed", entity::vector(lua, c.normal * c.speed)?)?;
        // before the impact it went into what it hit as fast as the impact took away
        t.raw_set("OurOldVelocity", entity::vector(lua, c.velocity + c.normal * c.speed)?)?;
        t.raw_set("OurNewVelocity", entity::vector(lua, c.velocity)?)?;
        t.raw_set("TheirOldVelocity", entity::vector(lua, c.other_velocity)?)?;
        t.raw_set("TheirNewVelocity", entity::vector(lua, c.other_velocity)?)?;
        t.raw_set("PhysObject", push_part(lua, c.entity, c.part)?)?;
        t.raw_set("HitObject", push_part(lua, c.other, c.other_part)?)?;
        t.raw_set("OurSurfaceProps", our as i64)?;
        t.raw_set("TheirSurfaceProps", their as i64)?;
        t.raw_set("DeltaTime", timer::TICK_INTERVAL)?;
        Ok(t)
    })();
    let Ok(data) = data else { return };
    let phys = push_part(lua, c.entity, c.part).unwrap_or(Value::Nil);
    crate::ents::call_method(lua, &ud, "PhysicsCollide", MultiValue::from_iter([Value::Table(data.clone()), phys]));
    if let Ok(callbacks) = entity::store_of(lua, &ud, "callbacks")
        && let Ok(Some(list)) = callbacks.raw_get::<Option<Table>>("PhysicsCollide")
    {
        for function in list.sequence_values::<mlua::Function>().filter_map(Result::ok) {
            let _ = crate::files::call_reporting(
                lua,
                function,
                MultiValue::from_iter([Value::UserData(ud.clone()), Value::Table(data.clone())]),
            );
        }
    }
}

/// The surface an entity's body is made of (`PhysObj:GetMaterial`), if it has a body.
pub(crate) fn material_of(lua: &Lua, handle: EntityHandle) -> Option<String> {
    let id = body(lua, handle)?;
    host(lua).physics.bodies.borrow().get(&id).map(|kept| kept.material.clone())
}

/// The `PhysObj` of an entity: valid while the entity has a body.
pub(crate) fn push(lua: &Lua, handle: EntityHandle) -> mlua::Result<Value> {
    push_part(lua, handle, 0)
}

/// A `PhysObj` of an entity by number (`GetPhysicsObjectNum`): `NULL` when it has none such.
pub(crate) fn push_part(lua: &Lua, handle: EntityHandle, part: usize) -> mlua::Result<Value> {
    let phys = match part_body(lua, handle, part) {
        Some(_) => PhysObjHandle(handle, part),
        None => PhysObjHandle(EntityHandle::NULL, 0),
    };
    Ok(Value::UserData(lua.create_userdata(phys)?))
}

/// Whether a `PhysObj` is valid.
pub(crate) fn valid(lua: &Lua, phys: PhysObjHandle) -> bool {
    host(lua).entities.is_valid(phys.0) && part_body(lua, phys.0, phys.1).is_some()
}

/// The entity and body of the `PhysObj` in argument 1, which must be valid.
fn this(a: &Args) -> mlua::Result<(EntityHandle, BodyId)> {
    let phys = a.userdata::<PhysObjHandle>(1, "PhysObj")?;
    match part_body(a.lua, phys.0, phys.1).filter(|_| host(a.lua).entities.is_valid(phys.0)) {
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

/// Where a `PhysObj` is, as an entity's position and angles: a part of a ragdoll where its
/// bone is, which is not where its body is when welded into another part's.
fn pose(a: &Args) -> mlua::Result<(Vector, Angle)> {
    let (handle, id) = this(a)?;
    let part = a.userdata::<PhysObjHandle>(1, "PhysObj")?.1;
    if let Some(found) = crate::ragdoll::part_pose(a.lua, handle, part) {
        return Ok(found);
    }
    let (origin, angles) = host(a.lua).physics.physics.borrow().pose(id).unwrap_or_default();
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
    def("GetMassCenter", |a| {
        let (handle, _) = this(a)?;
        let part = a.userdata::<PhysObjHandle>(1, "PhysObj")?.1;
        let center = vector(with(a, |p, id| p.mass_center(id))?);
        vector_value(a.lua, crate::ragdoll::in_part(a.lua, handle, part, center).unwrap_or(center))
    })?;
    def("GetVolume", |a| Ok(Value::Number(f64::from(with(a, |p, id| p.volume(id))?))))?;
    def("GetPos", |a| vector_value(a.lua, pose(a)?.0))?;
    def("GetAngles", |a| entity::angle(a.lua, pose(a)?.1))?;
    // a part of a ragdoll moves by itself; another body moves its entity
    def("SetPos", |a| {
        let (handle, _) = this(a)?;
        let part = a.userdata::<PhysObjHandle>(1, "PhysObj")?.1;
        let pos = a.userdata::<Vector>(2, "Vector")?;
        if !crate::ragdoll::set_part_pose(a.lua, handle, part, Some(pos), None) {
            entity::set_world_pos(a.lua, handle, pos);
        }
        Ok(Value::Nil)
    })?;
    def("SetAngles", |a| {
        let (handle, _) = this(a)?;
        let part = a.userdata::<PhysObjHandle>(1, "PhysObj")?.1;
        let angles = a.userdata::<Angle>(2, "Angle")?;
        if !crate::ragdoll::set_part_pose(a.lua, handle, part, None, Some(angles)) {
            entity::set_world_angles(a.lua, handle, angles);
        }
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
        set_material(a.lua, this(a)?.1, &material);
        Ok(Value::Nil)
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
    // what it touches now, and what both are made of: the world's surface where they touch
    def("GetFrictionSnapshot", |a| {
        let (handle, id) = this(a)?;
        let lua = a.lua;
        let contacts = host(lua).physics.physics.borrow().contacts(id);
        let own = material_of(lua, handle).unwrap_or_else(|| "default".to_string());
        let (own_index, _) = crate::surfaceprops::named(lua, &own);
        let list = lua.create_table()?;
        for contact in contacts {
            let point = vector(contact.point);
            let normal = vector(contact.normal);
            let (other, other_index) = match contact
                .other
                .and_then(|o| host(lua).physics.bodies.borrow().get(&o).map(|k| (k.entity, k.part, k.material.clone())))
            {
                Some((entity, part, material)) => (push_part(lua, entity, part)?, crate::surfaceprops::named(lua, &material).0),
                None => {
                    // the world: its surface under the contact
                    let sweep =
                        crate::trace::Sweep::line(point - normal * 4.0, point + normal * 4.0, crate::trace::Filter::Entities(vec![handle]));
                    let hit = crate::trace::trace(lua, &sweep);
                    (push(lua, EntityHandle::WORLD)?, crate::trace::surface_of(lua, &hit).0)
                }
            };
            let entry = lua.create_table()?;
            entry.raw_set("Other", other)?;
            entry.raw_set("Material", own_index as i64)?;
            entry.raw_set("MaterialOther", other_index as i64)?;
            entry.raw_set("ContactPoint", vector_value(lua, point)?)?;
            entry.raw_set("Normal", vector_value(lua, normal)?)?;
            entry.raw_set("NormalForce", f64::from(contact.force))?;
            list.raw_push(entry)?;
        }
        Ok(Value::Table(list))
    })?;
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
