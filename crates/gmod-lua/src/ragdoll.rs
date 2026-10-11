//! Ragdolls: a body for each solid of a model's physics model, held together by the joints its
//! `ragdollconstraint`s describe - `prop_ragdoll`, and the corpses of players.
//!
//! Each solid of the `.phy` belongs to the bone of its name and lies in that bone's space. The
//! parts start where their bones are as the entity plays when it is made, and fall from there.
//! A `ragdollconstraint` joins two solids at the child's bone, the child turning about its
//! parent within limits about the axes of its bone, measured from the model's rest pose
//! ([`gmod_physics::Joint::Ragdoll`]); one that lets nothing turn welds the child into its
//! parent's body. The parts hit each other, but for those joined and those that start in each
//! other; a `.phy` with `collisionrules` says which pairs alone do (`collisionpair`), or that
//! none do (`selfcollisions 0`).
//!
//! The entity is where its first part (part 0, the root of the model) is, its box around all
//! of them, and moving it moves them all. Drawn, a bone with a part is where that part is; one
//! without follows its parent as the entity plays it. Lua sees the parts as the entity's
//! `PhysObj`s by number (`GetPhysicsObjectNum`, `TranslatePhysBoneToBone`), each where its
//! bone is. A part welded into another's body is where it is on that body and is not moved by
//! itself: the first part of the body moves it (sandbox moves every part by the same offset).

use std::collections::HashMap;
use std::sync::Arc;

use gmod_model::Rig;
use gmod_model::pose::{self as rig_pose, IDENTITY, Matrix as BoneMatrix};
use gmod_physics::{Body, BodyId, Joint, Shape};
use mlua::Lua;

use crate::entity::{self, EntityHandle};
use crate::host;
use crate::matrix::Matrix;
use crate::physics::{angle, array, turn, vector};
use crate::vector::{Angle, Vector};

/// The parts of a ragdoll.
#[derive(Debug, Clone)]
pub(crate) struct Ragdoll {
    entity: EntityHandle,
    /// One for each solid of the physics model, in its order.
    parts: Vec<Part>,
}

#[derive(Debug, Clone, Copy)]
struct Part {
    body: BodyId,
    /// Its bone, when the model has one of its name.
    bone: Option<usize>,
    /// Where its bone is on its body: not where the body is when welded into another's.
    offset: BoneMatrix,
}

/// A place in the world as a matrix: its origin and the axes of its angles.
fn placed(origin: Vector, angles: Angle) -> BoneMatrix {
    let m = Matrix::from_angle(angles).m;
    let t = [origin.x, origin.y, origin.z];
    std::array::from_fn(|r| [m[r][0], m[r][1], m[r][2], t[r]])
}

/// The origin and angles of a matrix.
fn pose_of(m: &BoneMatrix) -> (Vector, Angle) {
    let angles = Matrix { m: [m[0], m[1], m[2], [0.0, 0.0, 0.0, 1.0]] }.angles();
    (Vector::new(m[0][3], m[1][3], m[2][3]), angles)
}

/// Where a body is, as a matrix.
fn body_at(lua: &Lua, id: BodyId) -> Option<BoneMatrix> {
    let (origin, angles) = host(lua).physics.physics.borrow().pose(id)?;
    Some(placed(vector(origin), angle(angles)))
}

/// Something of the ragdoll an entity is, if it is one.
fn get<R>(lua: &Lua, handle: EntityHandle, f: impl FnOnce(&Ragdoll) -> R) -> Option<R> {
    let host = host(lua);
    let ragdolls = host.physics.ragdolls.borrow();
    ragdolls.get(&handle.index?).filter(|r| r.entity == handle).map(f)
}

pub(crate) fn is_ragdoll(lua: &Lua, handle: EntityHandle) -> bool {
    get(lua, handle, |_| ()).is_some()
}

/// The body of a part of a ragdoll: none when the entity is no ragdoll, `Some(None)` when it
/// has no such part.
pub(crate) fn part_body(lua: &Lua, handle: EntityHandle, part: usize) -> Option<Option<BodyId>> {
    get(lua, handle, |r| r.parts.get(part).map(|p| p.body))
}

/// The bodies of a ragdoll, each once.
pub(crate) fn bodies(lua: &Lua, handle: EntityHandle) -> Option<Vec<BodyId>> {
    get(lua, handle, |r| {
        let mut out: Vec<BodyId> = Vec::new();
        for part in &r.parts {
            if !out.contains(&part.body) {
                out.push(part.body);
            }
        }
        out
    })
}

/// How many parts a ragdoll has.
pub(crate) fn count(lua: &Lua, handle: EntityHandle) -> Option<usize> {
    get(lua, handle, |r| r.parts.len())
}

/// The ragdoll of an entity goes; its bodies are taken away by [`crate::physics::destroy`].
pub(crate) fn forget(lua: &Lua, handle: EntityHandle) {
    let host = host(lua);
    let mut ragdolls = host.physics.ragdolls.borrow_mut();
    if let Some(index) = handle.index
        && ragdolls.get(&index).is_some_and(|r| r.entity == handle)
    {
        ragdolls.remove(&index);
    }
}

/// Makes an entity a ragdoll of its model, in place of the body it had, posed as it plays now
/// and moving as fast as `velocity`. Whether it became one: its model needs bones and a
/// physics model whose every part has a shape.
pub(crate) fn init(lua: &Lua, handle: EntityHandle, velocity: Vector) -> bool {
    // kept by the entity's number, without which its bodies could never be taken away
    let Some(index) = handle.index else { return false };
    crate::physics::destroy(lua, handle);
    let Some(model) = host(lua).entities.get(handle).map(|e| e.model.clone()) else { return false };
    let (Some(rig), Some(phy)) = (crate::anim::rig(lua, &model), crate::physics::phy(lua, &model)) else { return false };
    let Some((pos, angles)) = host(lua).entities.transform(handle) else { return false };
    let count = phy.solids.len();
    if count == 0 {
        return false;
    }
    let place = placed(pos, angles);
    let rest = rig.bone_to_model(&rig.rest());
    let now =
        crate::anim::model_bones(lua, handle).map(|(_, bones)| bones).filter(|b| b.len() == rest.len()).unwrap_or_else(|| rest.clone());
    let bones: Vec<Option<usize>> =
        phy.solids.iter().map(|s| rig.bones().iter().position(|b| b.name.eq_ignore_ascii_case(s.name()))).collect();
    // where each solid's space is in the model, at rest; one without a bone is the model's own
    let at_rest = |i: usize| bones[i].and_then(|b| rest.get(b)).copied().unwrap_or(IDENTITY);

    // a joint that lets nothing turn welds: the child goes into its parent's body, whose
    // first solid is where that body is
    let constraints = phy.ragdoll_constraints();
    let mut group: Vec<usize> = (0..count).collect();
    for c in constraints.iter().filter(|c| c.limits.iter().all(|l| l[0] == 0.0 && l[1] == 0.0)) {
        let (Some(&a), Some(&b)) = (group.get(c.parent), group.get(c.child)) else { continue };
        let (keep, gone) = (a.min(b), a.max(b));
        for g in group.iter_mut().filter(|g| **g == gone) {
            *g = keep;
        }
    }

    let mut made: HashMap<usize, BodyId> = HashMap::new();
    for first in (0..count).filter(|&i| group[i] == i) {
        let members: Vec<usize> = (0..count).filter(|&i| group[i] == first).collect();
        let to_body = rig_pose::invert(&at_rest(first));
        let hulls = members
            .iter()
            .flat_map(|&i| {
                let into = rig_pose::multiply(&to_body, &at_rest(i));
                phy.solids[i].convexes.iter().map(move |c| c.points.iter().map(|&p| rig_pose::transform(&into, p)).collect())
            })
            .collect();
        let shape = Shape::Hulls(hulls);
        let masses: Vec<f32> = members.iter().filter_map(|&i| phy.solids[i].mass()).collect();
        let solid = &phy.solids[first];
        let (origin, angles) = pose_of(&rig_pose::multiply(&place, &at_rest(first)));
        let body = Body {
            mass: (!masses.is_empty()).then(|| masses.iter().sum()),
            damping: solid.keys.number("damping").unwrap_or(0.0),
            rot_damping: solid.keys.number("rotdamping").unwrap_or(0.0),
            inertia: solid.keys.number("inertia").unwrap_or(1.0),
            ..Body::new(shape.clone(), array(origin), turn(angles))
        };
        let added = host(lua).physics.physics.borrow_mut().add(&body);
        let Some(id) = added else {
            // a part without a shape: no ragdoll
            for id in made.into_values() {
                crate::physics::remove_body(lua, id);
            }
            return false;
        };
        crate::physics::keep(lua, id, handle, first, &shape, solid.surface_prop().unwrap_or("default"));
        made.insert(first, id);
    }
    let parts: Vec<Part> = (0..count)
        .map(|i| Part {
            body: made[&group[i]],
            bone: bones[i],
            offset: rig_pose::multiply(&rig_pose::invert(&at_rest(group[i])), &at_rest(i)),
        })
        .collect();

    {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        for c in &constraints {
            let (Some(&a), Some(&b)) = (group.get(c.parent), group.get(c.child)) else { continue };
            if a == b {
                continue;
            }
            let (point, angles) = pose_of(&rig_pose::multiply(&place, &at_rest(c.child)));
            let joint = Joint::Ragdoll { point: array(point), angles: turn(angles), limits: c.limits, free: false };
            physics.join(made[&a], made[&b], joint, false, [0.0; 2]);
        }
        // which bodies hit each other: those the rules name, or without rules all but those
        // joined and those in each other at rest or as posed, which would only be thrown apart
        let pairs = phy.collision_pairs();
        let joined: Vec<[usize; 2]> = constraints.iter().filter_map(|c| Some([*group.get(c.parent)?, *group.get(c.child)?])).collect();
        let firsts: Vec<usize> = made.keys().copied().collect();
        let between = |a: usize, b: usize, [i, j]: [usize; 2]| (i == a && j == b) || (i == b && j == a);
        let mut hits: Vec<[usize; 2]> = Vec::new();
        for (n, &a) in firsts.iter().enumerate() {
            for &b in &firsts[n + 1..] {
                let hit = match &pairs {
                    Some(pairs) => pairs.iter().any(|&[i, j]| between(a, b, [group[i], group[j]])),
                    None => !joined.iter().any(|&p| between(a, b, p)) && !physics.overlapping(made[&a], made[&b]),
                };
                if hit {
                    hits.push([a, b]);
                } else {
                    physics.set_apart(made[&a], made[&b], true);
                }
            }
        }
        // the parts as the entity plays now, as fast as it went
        for (&first, &id) in &made {
            if let Some(bone) = bones[first].and_then(|b| now.get(b)) {
                let (origin, angles) = pose_of(&rig_pose::multiply(&place, bone));
                physics.set_pose(id, array(origin), turn(angles));
            }
            physics.set_velocity(id, array(velocity));
        }
        if pairs.is_none() {
            for [a, b] in hits {
                if physics.overlapping(made[&a], made[&b]) {
                    physics.set_apart(made[&a], made[&b], true);
                }
            }
        }
    }

    let root = parts[0].body;
    host(lua).physics.ragdolls.borrow_mut().insert(index, Ragdoll { entity: handle, parts });
    host(lua).entities.with_mut(handle, |ent| ent.physics = Some(root));
    follow(lua, handle);
    true
}

/// The entity goes where its first part is, its box around them all.
fn follow(lua: &Lua, handle: EntityHandle) {
    let Some(parts) = get(lua, handle, |r| r.parts.clone()) else { return };
    if let Some(root) = body_at(lua, parts[0].body) {
        let (pos, angles) = pose_of(&rig_pose::multiply(&root, &parts[0].offset));
        host(lua).entities.with_mut(handle, |ent| {
            if ent.parent.index.is_none() {
                ent.local_pos = pos;
                ent.local_angles = angles;
            }
        });
    }
    let Some((lo, hi)) = world_bounds(lua, handle) else { return };
    let Some((pos, angles)) = host(lua).entities.transform(handle) else { return };
    // the box in the entity's space around the box in the world
    let (mut mins, mut maxs) = (Vector::new(f32::MAX, f32::MAX, f32::MAX), Vector::new(f32::MIN, f32::MIN, f32::MIN));
    for i in 0..8 {
        let corner =
            Vector::new(if i & 1 == 0 { lo.x } else { hi.x }, if i & 2 == 0 { lo.y } else { hi.y }, if i & 4 == 0 { lo.z } else { hi.z });
        let local = entity::unrotated(corner - pos, angles);
        mins = Vector::new(mins.x.min(local.x), mins.y.min(local.y), mins.z.min(local.z));
        maxs = Vector::new(maxs.x.max(local.x), maxs.y.max(local.y), maxs.z.max(local.z));
    }
    host(lua).entities.with_mut(handle, |ent| {
        ent.mins = mins;
        ent.maxs = maxs;
    });
}

/// The box around all the parts of a ragdoll in the world.
pub(crate) fn world_bounds(lua: &Lua, handle: EntityHandle) -> Option<(Vector, Vector)> {
    let all = bodies(lua, handle)?;
    let host = host(lua);
    let physics = host.physics.physics.borrow();
    let mut found: Option<(Vector, Vector)> = None;
    for (lo, hi) in all.into_iter().filter_map(|id| physics.bounds(id)) {
        let (lo, hi) = (vector(lo), vector(hi));
        found = Some(match found {
            Some((a, b)) => {
                (Vector::new(a.x.min(lo.x), a.y.min(lo.y), a.z.min(lo.z)), Vector::new(b.x.max(hi.x), b.y.max(hi.y), b.z.max(hi.z)))
            }
            None => (lo, hi),
        });
    }
    found
}

/// The entity of a ragdoll was moved (`SetPos`, `SetAngles`): all its parts go along, as they
/// are to its first. Whether it is a ragdoll.
pub(crate) fn moved(lua: &Lua, handle: EntityHandle) -> bool {
    let Some(parts) = get(lua, handle, |r| r.parts.clone()) else { return false };
    let Some((pos, angles)) = host(lua).entities.transform(handle) else { return true };
    let Some(root) = body_at(lua, parts[0].body) else { return true };
    let was = rig_pose::multiply(&root, &parts[0].offset);
    let by = rig_pose::multiply(&placed(pos, angles), &rig_pose::invert(&was));
    for id in bodies(lua, handle).unwrap_or_default() {
        let Some(at) = body_at(lua, id) else { continue };
        let (origin, angles) = pose_of(&rig_pose::multiply(&by, &at));
        host(lua).physics.physics.borrow_mut().set_pose(id, array(origin), turn(angles));
    }
    follow(lua, handle);
    true
}

/// Where a part of a ragdoll is (`PhysObj:GetPos`, `GetAngles`): where its bone is, on its
/// body. None when the entity is no ragdoll or has no such part.
pub(crate) fn part_pose(lua: &Lua, handle: EntityHandle, part: usize) -> Option<(Vector, Angle)> {
    let part = get(lua, handle, |r| r.parts.get(part).copied())??;
    Some(pose_of(&rig_pose::multiply(&body_at(lua, part.body)?, &part.offset)))
}

/// Moves a part of a ragdoll by itself (`PhysObj:SetPos`, `SetAngles`) to a position and
/// angles, each as it is when not given; the first part takes the entity along. A part welded
/// into another's body stays on it, which the body's first part places. Whether the entity
/// is a ragdoll.
pub(crate) fn set_part_pose(lua: &Lua, handle: EntityHandle, part: usize, pos: Option<Vector>, angles: Option<Angle>) -> bool {
    let Some(parts) = get(lua, handle, |r| r.parts.clone()) else { return false };
    let Some(this) = parts.get(part) else { return true };
    if parts[..part].iter().any(|p| p.body == this.body) {
        return true;
    }
    let Some((was_pos, was_angles)) = part_pose(lua, handle, part) else { return true };
    let to = placed(pos.unwrap_or(was_pos), angles.unwrap_or(was_angles));
    let (origin, angles) = pose_of(&rig_pose::multiply(&to, &rig_pose::invert(&this.offset)));
    host(lua).physics.physics.borrow_mut().set_pose(this.body, array(origin), turn(angles));
    follow(lua, handle);
    true
}

/// A point in the space of a part's body in the part's own (`PhysObj:GetMassCenter`): another
/// for a part welded into another's body. None when the entity is no ragdoll or has no such
/// part.
pub(crate) fn in_part(lua: &Lua, handle: EntityHandle, part: usize, point: Vector) -> Option<Vector> {
    let offset = get(lua, handle, |r| r.parts.get(part).map(|p| p.offset))??;
    Some(vector(rig_pose::transform(&rig_pose::invert(&offset), array(point))))
}

/// After the bodies moved on: the ragdolls whose parts move take their entities along.
pub(crate) fn moved_on(lua: &Lua) {
    let moving: Vec<EntityHandle> = {
        let host = host(lua);
        let physics = host.physics.physics.borrow();
        let awake = physics.awake();
        let ragdolls = host.physics.ragdolls.borrow();
        ragdolls.values().filter(|r| r.parts.iter().any(|p| awake.contains(&p.body))).map(|r| r.entity).collect()
    };
    for handle in moving {
        follow(lua, handle);
    }
}

/// Where the bones of a ragdoll are, bone to model (the model drawn where the entity is): a
/// bone with a part where it is, others on their parents as `pose` has them. None when the
/// entity is no ragdoll.
pub(crate) fn bones(lua: &Lua, handle: EntityHandle, rig: &Arc<Rig>, pose: &[gmod_model::Local]) -> Option<Vec<BoneMatrix>> {
    let parts = get(lua, handle, |r| r.parts.clone())?;
    let (pos, angles) = host(lua).entities.transform(handle)?;
    let scale = host(lua).entities.get(handle).map_or(1.0, |e| e.model_scale);
    let mut place = placed(pos, angles);
    for row in &mut place {
        for value in row.iter_mut().take(3) {
            *value *= scale;
        }
    }
    let to_model = rig_pose::invert(&place);
    let played = rig.bone_to_model(pose);
    let mut out: Vec<BoneMatrix> = Vec::with_capacity(rig.bones().len());
    for (b, bone) in rig.bones().iter().enumerate() {
        let part = parts.iter().find(|p| p.bone == Some(b));
        let world = part.and_then(|p| Some(rig_pose::multiply(&body_at(lua, p.body)?, &p.offset)));
        let m = match (world, usize::try_from(bone.parent).ok().filter(|&p| p < b)) {
            (Some(world), _) => rig_pose::multiply(&to_model, &world),
            (None, Some(parent)) => match pose.get(b) {
                Some(local) => rig_pose::multiply(&out[parent], &rig_pose::local_matrix(local)),
                None => out[parent],
            },
            (None, None) => played.get(b).copied().unwrap_or(IDENTITY),
        };
        out.push(m);
    }
    Some(out)
}

/// The bone of a part (`TranslatePhysBoneToBone`): -1 for none. A body of one part is the
/// model's root bone.
pub(crate) fn bone_of_part(lua: &Lua, handle: EntityHandle, part: usize) -> i64 {
    match get(lua, handle, |r| r.parts.get(part).and_then(|p| p.bone)) {
        Some(bone) => bone.map_or(-1, |b| b as i64),
        None if part == 0 && crate::physics::body(lua, handle).is_some() => 0,
        None => -1,
    }
}

/// The part a bone moves with (`TranslateBoneToPhysBone`): its own, or the nearest of its
/// parents'; -1 for none. A body of one part moves all the bones.
pub(crate) fn part_of_bone(lua: &Lua, handle: EntityHandle, bone: i64) -> i64 {
    let Some(parts) = get(lua, handle, |r| r.parts.clone()) else {
        return if crate::physics::body(lua, handle).is_some() && bone >= 0 { 0 } else { -1 };
    };
    let model = host(lua).entities.get(handle).map(|e| e.model.clone()).unwrap_or_default();
    let Some(rig) = crate::anim::rig(lua, &model) else { return -1 };
    let mut at = usize::try_from(bone).ok();
    for _ in 0..rig.bones().len() {
        let Some(b) = at.filter(|&b| b < rig.bones().len()) else { break };
        if let Some(part) = parts.iter().position(|p| p.bone == Some(b)) {
            return part as i64;
        }
        at = usize::try_from(rig.bones()[b].parent).ok();
    }
    -1
}

/// Where a line from `start` to `end` first goes into a part of a ragdoll: how far along, the
/// face's normal and the part, the first of its body.
pub(crate) fn trace(lua: &Lua, handle: EntityHandle, start: Vector, end: Vector) -> Option<(f32, Vector, usize)> {
    let parts = get(lua, handle, |r| r.parts.clone())?;
    let host = host(lua);
    let physics = host.physics.physics.borrow();
    let mut best: Option<(f32, Vector, usize)> = None;
    for (n, part) in parts.iter().enumerate() {
        // a body welded of more parts is its first part's
        if parts[..n].iter().any(|p| p.body == part.body) {
            continue;
        }
        let Some(hit) = physics.ray_into(array(start), array(end), part.body) else { continue };
        if best.is_none_or(|(fraction, _, _)| hit.fraction < fraction) {
            best = Some((hit.fraction, vector(hit.normal), n));
        }
    }
    best
}
