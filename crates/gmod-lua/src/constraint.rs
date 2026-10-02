//! Constraints: the engine's entities that hold two bodies together, as joints of
//! [`gmod_physics`].
//!
//! constraint.lua - and so the tools of sandbox - makes them: it creates the entity, gives it
//! key-values and the two bodies (`SetPhysConstraintObjects`), and spawns and activates it.
//! A map names the two entities instead (`attach1`, `attach2`; none is the world), and its
//! constraints are activated once all its entities have spawned. The joint is made when the
//! constraint is activated, as the bodies are then; its points are where the entity is
//! (`SetPos`) and where its key-values say, in the world.
//!
//! - `phys_constraint`: a weld.
//! - `phys_ballsocket`: turning any way about where it is.
//! - `phys_hinge`: turning about the line from where it is to `hingeaxis`.
//! - `phys_slideconstraint`: sliding along the line from where it is to `slideaxis`.
//! - `phys_lengthconstraint`: a rope from where it is to `attachpoint`, `length` long; rigid
//!   (spawn flag 2), it does not get shorter either.
//! - `phys_spring`: a spring from where it is to `springaxis`, `length` long (0: as far as
//!   they are), `constant` stiff and `damping` damped. It pulls them together and pushes them
//!   apart: "stretch only" (spawn flag 1) is not kept yet.
//! - `phys_ragdollconstraint`: a ball and socket; its limits are not kept yet.
//! - `logic_collision_pair`: the two go through each other while it is disabled
//!   (`startdisabled`, the input `DisableCollisions`) until `EnableCollisions`. Removed, it
//!   leaves them as they were, unless `disable_on_remove` says to let them collide again.
//!
//! Spawn flag 1 lets the two go through each other (springs aside), and 4 waits for `TurnOn`.
//! A constraint pulled harder than `forcelimit`, or twisted harder than `torquelimit` - the
//! kilograms whose weight that is, 0 for never - breaks, and so does one given `Break`: its
//! joint goes and the entity is removed. `TurnOff` takes the joint away until `TurnOn`.
//!
//! `phys_keepupright`, the motors (`phys_torque`, `phys_motor`), `phys_pulleyconstraint`,
//! `phys_constraintsystem` and the rope that is seen (`keyframe_rope`) are made, but hold
//! nothing yet. Every constraint is solved with every other: `SetPhysConstraintSystem` has
//! nothing to choose.

use gmod_physics::{BodyId, Joint, JointId};
use mlua::{Lua, MultiValue, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle, PhysObjHandle};
use crate::host;
use crate::vector::Vector;

/// The classes that make joints.
const JOINTS: &[&str] = &[
    "phys_constraint",
    "phys_ballsocket",
    "phys_hinge",
    "phys_slideconstraint",
    "phys_lengthconstraint",
    "phys_spring",
    "phys_ragdollconstraint",
];

const COLLISION_PAIR: &str = "logic_collision_pair";

/// The classes `IsConstraint` says are constraints.
const CONSTRAINTS: &[&str] = &[
    "phys_constraint",
    "phys_ballsocket",
    "phys_hinge",
    "phys_slideconstraint",
    "phys_lengthconstraint",
    "phys_ragdollconstraint",
    "phys_pulleyconstraint",
    "phys_keepupright",
];

/// The two go through each other.
const SF_NO_COLLISION: i64 = 1;
/// A rope that does not get shorter.
const SF_RIGID: i64 = 2;
/// Not held until `TurnOn`.
const SF_START_INACTIVE: i64 = 4;

fn is_one_of(class: &str, classes: &[&str]) -> bool {
    classes.iter().any(|c| c.eq_ignore_ascii_case(class))
}

fn class_of(lua: &Lua, handle: EntityHandle) -> Option<String> {
    host(lua).entities.get(handle).filter(|ent| !ent.scripted).map(|ent| ent.class.to_ascii_lowercase())
}

/// The body of what a constraint holds: the world's when it is the world or nothing.
fn body_of(lua: &Lua, owner: EntityHandle) -> Option<BodyId> {
    let owner = if host(lua).entities.is_valid(owner) { owner } else { EntityHandle::WORLD };
    crate::physics::body(lua, owner)
}

/// What a constraint holds: what `SetPhysConstraintObjects` gave it, else the entities its
/// key-values name.
fn owners(lua: &Lua, handle: EntityHandle) -> [EntityHandle; 2] {
    let (given, names) = {
        let host = host(lua);
        let Some(ent) = host.entities.get(handle) else { return [EntityHandle::NULL; 2] };
        let name = |key: &str| ent.keyvalue(key).filter(|n| !n.is_empty()).map(str::to_string);
        (ent.constrained, [name("attach1"), name("attach2")])
    };
    if let Some(given) = given {
        return given;
    }
    names.map(|name| match name {
        Some(name) => crate::ents::named(lua, &name).unwrap_or(EntityHandle::NULL),
        None => EntityHandle::WORLD,
    })
}

/// Makes a constraint's joint, as the bodies are now, unless it has one. Nothing is held when
/// a body is missing, or both are the same.
fn make(lua: &Lua, handle: EntityHandle) {
    let Some(class) = class_of(lua, handle) else { return };
    let holds = host(lua).entities.get(handle).is_some_and(|ent| ent.joint.is_none_or(|j| !host(lua).physics.physics.borrow().joined(j)));
    if !holds {
        return;
    }
    let owners = owners(lua, handle);
    let (Some(first), Some(second)) = (body_of(lua, owners[0]), body_of(lua, owners[1])) else { return };
    if first == second {
        return;
    }
    let made = {
        let host = host(lua);
        let Some(ent) = host.entities.get(handle) else { return };
        let number = |key: &str| ent.keyvalue(key).map_or(0.0, |v| crate::convar::atof(v) as f32);
        let point = |key: &str| {
            let [x, y, z] = crate::ents::three(ent.keyvalue(key).unwrap_or(""));
            [x, y, z]
        };
        let Some((pos, _)) = host.entities.transform(handle) else { return };
        let at = [pos.x, pos.y, pos.z];
        let axis = |key: &str| {
            let to = point(key);
            [to[0] - at[0], to[1] - at[1], to[2] - at[2]]
        };
        let mut collide = ent.spawn_flags & SF_NO_COLLISION == 0;
        let joint = match class.as_str() {
            "phys_constraint" => Joint::Weld,
            "phys_ballsocket" | "phys_ragdollconstraint" => Joint::Ball { point: at },
            "phys_hinge" => Joint::Hinge { point: at, axis: axis("hingeaxis") },
            "phys_slideconstraint" => Joint::Slider { point: at, axis: axis("slideaxis") },
            "phys_lengthconstraint" => {
                Joint::Rope { start: at, end: point("attachpoint"), length: number("length"), rigid: ent.spawn_flags & SF_RIGID != 0 }
            }
            "phys_spring" => {
                let end = point("springaxis");
                let length = match number("length") {
                    length if length > 0.0 => length,
                    _ => (Vector::new(end[0], end[1], end[2]) - pos).length(),
                };
                collide = true;
                Joint::Spring { start: at, end, length, stiffness: number("constant"), damping: number("damping") }
            }
            COLLISION_PAIR => {
                collide = false;
                Joint::NoCollide
            }
            _ => return,
        };
        let strength = [number("forcelimit"), number("torquelimit")];
        drop(ent);
        let mut physics = host.physics.physics.borrow_mut();
        let made = physics.join(first, second, joint, collide, strength);
        physics.wake(first);
        physics.wake(second);
        made
    };
    host(lua).entities.with_mut(handle, |ent| ent.joint = made);
}

/// Takes a constraint's joint away.
fn release(lua: &Lua, handle: EntityHandle) {
    let joint = host(lua).entities.with_mut(handle, |ent| ent.joint.take()).flatten();
    if let Some(joint) = joint {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        physics.unjoin(joint);
        let owners = owners(lua, handle);
        for body in owners.into_iter().filter_map(|o| body_of(lua, o)) {
            physics.wake(body);
        }
    }
}

/// `ent:Activate()`: a constraint takes hold, unless it waits for `TurnOn`; a collision pair
/// that starts disabled lets the two through.
pub(crate) fn activate(lua: &Lua, handle: EntityHandle) {
    let Some(class) = class_of(lua, handle) else { return };
    let (flags, disabled) = match host(lua).entities.get(handle) {
        Some(ent) => (ent.spawn_flags, ent.keyvalue("startdisabled").is_some_and(|v| crate::convar::atof(v) != 0.0)),
        None => return,
    };
    if (is_one_of(&class, JOINTS) && flags & SF_START_INACTIVE == 0) || (class == COLLISION_PAIR && disabled) {
        make(lua, handle);
    }
}

/// An input a constraint knows. Whether it was taken.
pub(crate) fn input(lua: &Lua, handle: EntityHandle, name: &str) -> bool {
    let Some(class) = class_of(lua, handle) else { return false };
    let name = name.to_ascii_lowercase();
    if class == COLLISION_PAIR {
        match name.as_str() {
            "disablecollisions" => make(lua, handle),
            "enablecollisions" => release(lua, handle),
            _ => return false,
        }
        return true;
    }
    if is_one_of(&class, JOINTS) {
        match name.as_str() {
            "turnon" => make(lua, handle),
            "turnoff" => release(lua, handle),
            "break" => {
                release(lua, handle);
                crate::ents::remove(lua, handle);
            }
            _ => return false,
        }
        return true;
    }
    // the motors take what constraint.lua gives them, and do nothing yet
    is_one_of(&class, &["phys_torque", "phys_motor"]) && matches!(name.as_str(), "scale" | "activate" | "deactivate" | "turnon" | "turnoff")
}

/// A constraint is removed: its joint goes - a collision pair's only when it says so.
pub(crate) fn removed(lua: &Lua, handle: EntityHandle) {
    let Some(class) = class_of(lua, handle) else { return };
    let keep = class == COLLISION_PAIR
        && !host(lua)
            .entities
            .get(handle)
            .is_some_and(|ent| ent.keyvalue("disable_on_remove").is_some_and(|v| crate::convar::atof(v) != 0.0));
    if keep {
        host(lua).entities.with_mut(handle, |ent| ent.joint = None);
    } else {
        release(lua, handle);
    }
}

/// The joints that broke this tick: their constraints are removed.
pub(crate) fn broken(lua: &Lua, joints: &[JointId]) {
    if joints.is_empty() {
        return;
    }
    let gone: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|&h| entities.get(h).is_some_and(|ent| ent.joint.is_some_and(|j| joints.contains(&j))))
            .collect()
    };
    for handle in gone {
        host(lua).entities.with_mut(handle, |ent| ent.joint = None);
        crate::ents::remove(lua, handle);
    }
}

/// What the constraint in argument 1 holds, the world for nothing.
fn held(a: &Args) -> mlua::Result<[EntityHandle; 2]> {
    let handle = entity::this(a, 1)?;
    Ok(owners(a.lua, handle).map(|o| if host(a.lua).entities.is_valid(o) { o } else { EntityHandle::WORLD }))
}

/// The methods of constraints that give two values: what they hold, as entities and as
/// `PhysObj`s.
pub(crate) fn install_many(def: &entity::DefMany) -> mlua::Result<()> {
    def("GetConstrainedEntities", |a| {
        let [first, second] = held(a)?;
        Ok(MultiValue::from_iter([entity::push(a.lua, first)?, entity::push(a.lua, second)?]))
    })?;
    def("GetConstrainedPhysObjects", |a| {
        let [first, second] = held(a)?;
        Ok(MultiValue::from_iter([crate::physics::push(a.lua, first)?, crate::physics::push(a.lua, second)?]))
    })?;
    Ok(())
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().raw_set("SetPhysConstraintSystem", lua.create_function(|_, _: MultiValue| Ok(()))?)
}

/// The methods of constraints.
pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("SetPhysConstraintObjects", |a| {
        let handle = entity::this(a, 1)?;
        let first = a.userdata::<PhysObjHandle>(2, "PhysObj")?.0;
        let second = a.userdata::<PhysObjHandle>(3, "PhysObj")?.0;
        host(a.lua).entities.with_mut(handle, |ent| ent.constrained = Some([first, second]));
        Ok(Value::Nil)
    })?;
    def("IsConstraint", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(class_of(a.lua, handle).is_some_and(|class| is_one_of(&class, CONSTRAINTS))))
    })?;
    Ok(())
}
