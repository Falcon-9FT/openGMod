//! Hitboxes: the boxes on the bones of a model that shots hit (`gmod_model::HitboxSet`). A
//! trace that asks for them (`CONTENTS_HITBOX`, as `MASK_SHOT` does) goes through a player's
//! or an NPC's hitboxes where its bones are now, not through its box: it says which box it hit
//! (`HitBox`) and of what group (`HitGroup`: head, chest, a leg...), and it misses between the
//! legs. One set of a model's is in use, the first unless Lua chooses (`SetHitboxSet`).

use std::sync::Arc;

use gmod_model::pose::{self as rig_pose, Matrix as BoneMatrix};
use gmod_model::{Hitbox, Rig};
use mlua::{MultiValue, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle, Kind};
use crate::host;
use crate::trace::{BoxHit, through_box};
use crate::vector::Vector;

/// `HITGROUP_*`.
pub mod group {
    pub const GENERIC: i32 = 0;
    pub const HEAD: i32 = 1;
    pub const CHEST: i32 = 2;
    pub const STOMACH: i32 = 3;
    pub const LEFTARM: i32 = 4;
    pub const RIGHTARM: i32 = 5;
    pub const LEFTLEG: i32 = 6;
    pub const RIGHTLEG: i32 = 7;
    pub const GEAR: i32 = 10;
}

/// How far a model's hitboxes may reach out of its box: what a trace must pass to look at them.
const REACH: f32 = 32.0;

/// What a trace hit of a model's hitboxes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HitboxHit {
    pub fraction: f32,
    pub normal: Vector,
    pub start_solid: bool,
    pub hitbox: usize,
    pub group: i32,
    pub bone: usize,
}

/// The model of an entity, and the number of its set of hitboxes in use (always one there is).
fn set_of(lua: &mlua::Lua, handle: EntityHandle) -> Option<(Arc<Rig>, usize)> {
    let (model, set) = host(lua).entities.get(handle).map(|e| (e.model.clone(), e.hitbox_set))?;
    let rig = crate::anim::rig(lua, &model)?;
    let count = rig.mdl().hitbox_sets.len();
    (count > 0).then_some((rig, set.min(count - 1)))
}

/// The hitboxes of the set in use.
fn hitboxes(rig: &Rig, set: usize) -> &[Hitbox] {
    rig.mdl().hitbox_sets.get(set).map_or(&[], |s| s.hitboxes.as_slice())
}

/// Whether traces for hitboxes go through an entity's: a player's or an NPC's that has some.
pub(crate) fn traced(lua: &mlua::Lua, handle: EntityHandle) -> bool {
    let kind = host(lua).entities.get(handle).map(|e| e.kind);
    matches!(kind, Some(Kind::Player | Kind::Npc | Kind::NextBot))
        && set_of(lua, handle).is_some_and(|(rig, set)| !hitboxes(&rig, set).is_empty())
}

/// Where a ray from `start` to `end` first goes into one of an entity's hitboxes, if it does.
pub(crate) fn trace(lua: &mlua::Lua, handle: EntityHandle, start: Vector, end: Vector) -> Option<HitboxHit> {
    // far from its box, none of its hitboxes is looked at
    let (pos, angles) = host(lua).entities.transform(handle)?;
    let (mins, maxs) = host(lua).entities.get(handle).map(|e| (e.mins, e.maxs))?;
    let (lo, hi) = entity::rotated_bounds(mins, maxs, angles);
    let reach = Vector::new(REACH, REACH, REACH);
    through_box(start, end, pos + lo - reach, pos + hi + reach)?;
    let (rig, set) = set_of(lua, handle)?;
    let (_, bones) = crate::anim::world_bones(lua, handle)?;
    let mut best: Option<HitboxHit> = None;
    for (i, hitbox) in hitboxes(&rig, set).iter().enumerate() {
        let Some(world) = bones.get(hitbox.bone) else { continue };
        let Some(hit) = through_hitbox(world, hitbox, start, end) else { continue };
        if best.is_none_or(|b| hit.fraction < b.fraction) {
            best = Some(HitboxHit {
                fraction: hit.fraction,
                normal: hit.normal,
                start_solid: hit.start_solid,
                hitbox: i,
                group: hitbox.group,
                bone: hitbox.bone,
            });
        }
    }
    best
}

/// A ray through one hitbox on a bone where `world` puts it: in the box's own space, where it
/// is upright, the fraction the same.
fn through_hitbox(world: &BoneMatrix, hitbox: &Hitbox, start: Vector, end: Vector) -> Option<BoxHit> {
    let to_box = rig_pose::invert(world);
    let local = |p: Vector| {
        let v = rig_pose::transform(&to_box, [p.x, p.y, p.z]);
        Vector::new(v[0], v[1], v[2])
    };
    let v = |a: [f32; 3]| Vector::new(a[0], a[1], a[2]);
    let hit = through_box(local(start), local(end), v(hitbox.mins), v(hitbox.maxs))?;
    // the face's normal turned back into the world
    let n = hit.normal;
    let turned = Vector::new(
        world[0][0] * n.x + world[0][1] * n.y + world[0][2] * n.z,
        world[1][0] * n.x + world[1][1] * n.y + world[1][2] * n.z,
        world[2][0] * n.x + world[2][1] * n.y + world[2][2] * n.z,
    );
    Some(BoxHit { normal: if turned.length() > 0.0 { turned.normalized() } else { turned }, ..hit })
}

fn integer(n: i64) -> mlua::Result<Value> {
    Ok(Value::Integer(n))
}

/// The hitbox in argument 2 of the set in argument 3 (the set in use when not given).
fn hitbox_arg(a: &Args) -> mlua::Result<Option<Hitbox>> {
    let handle = entity::this(a, 1)?;
    let Some((rig, current)) = set_of(a.lua, handle) else { return Ok(None) };
    let index = a.number(2)? as i64;
    let set = a.opt_number(3)?.map_or(current, |s| s.max(0.0) as usize);
    let boxes = rig.mdl().hitbox_sets.get(set).map_or(&[][..], |s| s.hitboxes.as_slice());
    Ok(usize::try_from(index).ok().and_then(|i| boxes.get(i)).cloned())
}

pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("GetHitBoxCount", |a| {
        let handle = entity::this(a, 1)?;
        let Some((rig, current)) = set_of(a.lua, handle) else { return integer(0) };
        let set = a.opt_number(2)?.map_or(current, |s| s.max(0.0) as usize);
        integer(rig.mdl().hitbox_sets.get(set).map_or(0, |s| s.hitboxes.len()) as i64)
    })?;
    def("GetHitBoxBone", |a| match hitbox_arg(a)? {
        Some(h) => integer(h.bone as i64),
        None => Ok(Value::Nil),
    })?;
    def("GetHitBoxHitGroup", |a| match hitbox_arg(a)? {
        Some(h) => integer(h.group.into()),
        None => Ok(Value::Nil),
    })?;
    def("GetHitboxSetCount", |a| {
        let handle = entity::this(a, 1)?;
        integer(set_of(a.lua, handle).map_or(0, |(rig, _)| rig.mdl().hitbox_sets.len() as i64))
    })?;
    def("SetHitboxSet", |a| {
        let handle = entity::this(a, 1)?;
        let Some((rig, _)) = set_of(a.lua, handle) else { return Ok(Value::Nil) };
        let sets = &rig.mdl().hitbox_sets;
        let set = match a.get(2) {
            Some(Value::String(name)) => {
                let name = name.to_string_lossy();
                sets.iter().position(|s| s.name.eq_ignore_ascii_case(&name))
            }
            _ => usize::try_from(a.number(2)? as i64).ok().filter(|&s| s < sets.len()),
        };
        if let Some(set) = set {
            host(a.lua).entities.with_mut(handle, |ent| ent.hitbox_set = set);
        }
        Ok(Value::Nil)
    })?;
    Ok(())
}

pub(crate) fn install_many(def: &entity::DefMany) -> mlua::Result<()> {
    def("GetHitBoxBounds", |a| {
        let Some(h) = hitbox_arg(a)? else { return Ok(MultiValue::new()) };
        let v = |p: [f32; 3]| entity::vector(a.lua, Vector::new(p[0], p[1], p[2]));
        Ok(MultiValue::from_iter([v(h.mins)?, v(h.maxs)?]))
    })?;
    def("GetHitboxSet", |a| {
        let handle = entity::this(a, 1)?;
        let Some((rig, set)) = set_of(a.lua, handle) else { return Ok(MultiValue::new()) };
        let name = rig.mdl().hitbox_sets[set].name.clone();
        Ok(MultiValue::from_iter([Value::Integer(set as i64), Value::String(a.lua.create_string(&name)?)]))
    })?;
    Ok(())
}
