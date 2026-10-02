//! Traces as `util.TraceLine` and `util.TraceHull` make them: through the world of the map -
//! what [`gmod_bsp::Collision`] has, brushes, displacements, the brush entities where the map
//! put them and the static props - and through the boxes of the solid entities: a prop's box
//! turned with it for a ray, the box around it for a hull. Players and NPCs are
//! `CONTENTS_MONSTER`, everything else `CONTENTS_SOLID`. A trace has a collision group
//! (`collisiongroup`, a player's movement `COLLISION_GROUP_PLAYER_MOVEMENT`), and the
//! entities whose group does not collide with it (`should_collide`) are not hit; debris only
//! by a mask with `CONTENTS_DEBRIS`.
//!
//! What was hit is made of a surface ([`crate::surfaceprops`]): the map's material's
//! `$surfaceprop`, an entity's body's; `SurfaceProps` is its number, `MatType` its `MAT_*`.
//!
//! Not yet: the shapes of models (physics, stage 4), hit boxes and hit groups, the
//! `ShouldCollide` hook.

use std::cell::RefCell;
use std::sync::Arc;

use gmod_bsp::collision::EPSILON;
use gmod_bsp::{Collision, contents, mask, surf};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle, Kind};
use crate::vector::Vector;
use crate::{files, host};

/// `MAT_DEFAULT`: what is hit when it has no surface of its own.
const MAT_DEFAULT: i64 = 88;
/// How far a trace goes for `GetEyeTrace`: `4096 * 8`, as `util.GetPlayerTrace`.
pub const EYE_TRACE_LENGTH: f32 = 32768.0;
/// `FSOLID_NOT_SOLID` and `FSOLID_TRIGGER`: entities traces go through.
const NOT_SOLID: i64 = 4 | 8;

/// `COLLISION_GROUP_`s.
pub mod group {
    pub const NONE: i64 = 0;
    pub const DEBRIS: i64 = 1;
    pub const DEBRIS_TRIGGER: i64 = 2;
    pub const INTERACTIVE_DEBRIS: i64 = 3;
    pub const INTERACTIVE: i64 = 4;
    pub const PLAYER: i64 = 5;
    pub const BREAKABLE_GLASS: i64 = 6;
    pub const VEHICLE: i64 = 7;
    pub const PLAYER_MOVEMENT: i64 = 8;
    pub const NPC: i64 = 9;
    pub const IN_VEHICLE: i64 = 10;
    pub const WEAPON: i64 = 11;
    pub const VEHICLE_CLIP: i64 = 12;
    pub const PROJECTILE: i64 = 13;
    pub const DOOR_BLOCKER: i64 = 14;
    pub const PASSABLE_DOOR: i64 = 15;
    pub const DISSOLVING: i64 = 16;
    pub const PUSHAWAY: i64 = 17;
}

/// Whether things of two collision groups collide, as Half-Life 2's game rules have it: a
/// player's movement is the player, players go through weapons, pushaway things and
/// interactive debris, debris collides with nothing but `COLLISION_GROUP_NONE`, and so on.
pub(crate) fn should_collide(a: i64, b: i64) -> bool {
    let remap = |g: i64| if g == group::PLAYER_MOVEMENT { group::PLAYER } else { g };
    let (a, b) = (remap(a), remap(b));
    let (g0, g1) = if a > b { (b, a) } else { (a, b) };
    if g0 == group::PLAYER && g1 == group::PUSHAWAY {
        return false;
    }
    if g0 == group::DEBRIS && g1 == group::PUSHAWAY {
        return true;
    }
    if g0 == group::IN_VEHICLE || g1 == group::IN_VEHICLE {
        return false;
    }
    if g1 == group::DOOR_BLOCKER && g0 != group::NPC {
        return false;
    }
    if g0 == group::PLAYER && g1 == group::PASSABLE_DOOR {
        return false;
    }
    if g0 == group::DEBRIS || g0 == group::DEBRIS_TRIGGER {
        return false;
    }
    if (g0 == group::DISSOLVING || g1 == group::DISSOLVING) && g0 != group::NONE {
        return false;
    }
    if g0 == group::INTERACTIVE_DEBRIS && (g1 == group::INTERACTIVE_DEBRIS || g1 == group::PLAYER) {
        return false;
    }
    if g0 == group::BREAKABLE_GLASS && g1 == group::BREAKABLE_GLASS {
        return false;
    }
    if g1 == group::INTERACTIVE && g0 != group::NONE {
        return false;
    }
    if g1 == group::PROJECTILE && matches!(g0, group::DEBRIS | group::WEAPON | group::PROJECTILE) {
        return false;
    }
    if g1 == group::WEAPON && matches!(g0, group::VEHICLE | group::PLAYER | group::NPC) {
        return false;
    }
    if g0 == group::VEHICLE_CLIP || g1 == group::VEHICLE_CLIP {
        return g0 == group::VEHICLE;
    }
    true
}

/// The world of the map traces go through, once a map is loaded.
#[derive(Default)]
pub(crate) struct World {
    pub collision: RefCell<Option<Arc<Collision>>>,
    /// The material of each surface (`texinfo`).
    pub textures: RefCell<Vec<String>>,
}

/// What stops a trace besides the world: which entities it may hit.
pub(crate) enum Filter {
    None,
    /// These entities are not hit.
    Entities(Vec<EntityHandle>),
    /// Entities of these classes are not hit.
    Classes(Vec<String>),
    /// A Lua function says, for each entity, whether it is hit.
    Function(Function),
}

/// A trace to make.
pub(crate) struct Sweep {
    pub start: Vector,
    pub end: Vector,
    pub mins: Vector,
    pub maxs: Vector,
    pub mask: u32,
    pub filter: Filter,
    /// The filter names the only entities that are hit.
    pub whitelist: bool,
    pub ignore_world: bool,
    /// The collision group of what traces: entities of groups that do not collide with it
    /// are not hit.
    pub group: i64,
}

impl Sweep {
    pub fn line(start: Vector, end: Vector, filter: Filter) -> Sweep {
        Sweep {
            start,
            end,
            mins: Vector::default(),
            maxs: Vector::default(),
            mask: mask::SOLID,
            filter,
            whitelist: false,
            ignore_world: false,
            group: group::NONE,
        }
    }
}

/// What a trace hit.
#[derive(Debug, Clone)]
pub(crate) struct Hit {
    pub fraction: f32,
    pub end: Vector,
    pub normal: Vector,
    pub start_solid: bool,
    pub all_solid: bool,
    pub fraction_left_solid: f32,
    pub contents: u32,
    /// The entity hit: the world for brushes and static props, `NULL` for nothing.
    pub entity: EntityHandle,
    pub texture: String,
    pub surface_flags: i32,
    pub displacement: bool,
}

impl Hit {
    fn hit(&self) -> bool {
        self.fraction < 1.0 || self.start_solid
    }
}

fn vector(v: [f32; 3]) -> Vector {
    Vector::new(v[0], v[1], v[2])
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

/// The entity of the map that a brush model belongs to: the one made from that entry of the
/// entity lump, else the world.
fn brush_entity(lua: &Lua, collision: &Collision, model: usize) -> EntityHandle {
    let placed = collision.placements.iter().find(|p| p.model == model).and_then(|p| p.entity);
    let found = placed.and_then(|i| {
        let id = crate::ents::FIRST_MAP_CREATION_ID + i as i32;
        let entities = &host(lua).entities;
        entities.handles().into_iter().find(|&h| entities.get(h).is_some_and(|ent| ent.map_creation_id == Some(id)))
    });
    found.unwrap_or(EntityHandle::WORLD)
}

/// Whether the filter lets the trace hit an entity.
fn allowed(lua: &Lua, sweep: &Sweep, handle: EntityHandle) -> bool {
    let named = match &sweep.filter {
        Filter::None => return true,
        Filter::Entities(list) => list.contains(&handle),
        Filter::Classes(classes) => {
            host(lua).entities.get(handle).is_some_and(|ent| classes.iter().any(|c| c.eq_ignore_ascii_case(&ent.class)))
        }
        Filter::Function(f) => {
            let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { return false };
            // the function says what is hit, whitelist or not
            return match files::call_reporting(lua, f.clone(), MultiValue::from_iter([Value::UserData(ud)])) {
                Ok(result) => !matches!(result.front(), None | Some(Value::Nil) | Some(Value::Boolean(false))),
                Err(_) => false,
            };
        }
    };
    named == sweep.whitelist
}

/// Where a segment enters a box: the fraction, the normal of the face, or that it starts
/// inside (and the fraction where it leaves, 1 when it never does).
struct BoxHit {
    fraction: f32,
    normal: Vector,
    start_solid: bool,
    left: f32,
}

fn through_box(start: Vector, end: Vector, lo: Vector, hi: Vector) -> Option<BoxHit> {
    let (mut enter, mut leave) = (f32::MIN, f32::MAX);
    let mut normal = Vector::default();
    let axes = [(start.x, end.x, lo.x, hi.x), (start.y, end.y, lo.y, hi.y), (start.z, end.z, lo.z, hi.z)];
    for (axis, (s, e, lo, hi)) in axes.into_iter().enumerate() {
        let d = e - s;
        if d.abs() < 1e-6 {
            if s < lo || s > hi {
                return None;
            }
            continue;
        }
        let (near, far, sign) = if d > 0.0 { ((lo - s) / d, (hi - s) / d, -1.0) } else { ((hi - s) / d, (lo - s) / d, 1.0) };
        if near > enter {
            enter = near;
            let mut n = [0.0; 3];
            n[axis] = sign;
            normal = vector(n);
        }
        leave = leave.min(far);
    }
    if enter > leave || leave < 0.0 || enter > 1.0 {
        return None;
    }
    if enter < 0.0 {
        return Some(BoxHit { fraction: 0.0, normal: Vector::default(), start_solid: true, left: leave.min(1.0) });
    }
    // stop a little before the face, as traces through the world do
    let length = (end - start).length();
    let back = if length > 0.0 { EPSILON / length } else { 0.0 };
    Some(BoxHit { fraction: (enter - back).max(0.0), normal, start_solid: false, left: 0.0 })
}

/// Makes a trace.
pub(crate) fn trace(lua: &Lua, sweep: &Sweep) -> Hit {
    let mut best = Hit {
        fraction: 1.0,
        end: sweep.end,
        normal: Vector::default(),
        start_solid: false,
        all_solid: false,
        fraction_left_solid: 0.0,
        contents: 0,
        entity: EntityHandle::NULL,
        texture: "**empty**".into(),
        surface_flags: 0,
        displacement: false,
    };
    let collision = host(lua).world.collision.borrow().clone();
    if let Some(collision) = collision.filter(|_| !sweep.ignore_world) {
        let t = collision.trace(array(sweep.start), array(sweep.end), array(sweep.mins), array(sweep.maxs), sweep.mask);
        if t.hit() {
            let entity = match (t.static_prop, t.model) {
                (Some(_), _) | (None, Some(0)) | (None, None) => EntityHandle::WORLD,
                (None, Some(model)) => brush_entity(lua, &collision, model),
            };
            if entity == EntityHandle::WORLD || allowed(lua, sweep, entity) {
                let texture = if t.static_prop.is_some() {
                    "**studio**".to_string()
                } else if t.displacement {
                    "**displacement**".to_string()
                } else {
                    t.texinfo.and_then(|i| host(lua).world.textures.borrow().get(i).cloned()).unwrap_or_default()
                };
                best = Hit {
                    fraction: t.fraction,
                    end: vector(t.end),
                    normal: vector(t.normal),
                    start_solid: t.start_solid,
                    all_solid: t.all_solid,
                    fraction_left_solid: t.fraction_left_solid,
                    contents: t.contents,
                    entity,
                    texture,
                    surface_flags: t.surface_flags,
                    displacement: t.displacement,
                };
            }
        }
    }
    if best.start_solid {
        return best;
    }

    // the boxes of the solid entities
    let hull = sweep.mins != Vector::default() || sweep.maxs != Vector::default();
    let candidates: Vec<(EntityHandle, u32)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|h| h.index != Some(0))
            .filter_map(|h| {
                let ent = entities.get(h)?;
                let solid = !ent.removed && ent.solid != 0 && ent.solid_flags & NOT_SOLID == 0 && !ent.model.starts_with('*');
                let contents =
                    if matches!(ent.kind, Kind::Player | Kind::Npc | Kind::NextBot) { contents::MONSTER } else { contents::SOLID };
                let debris_hit = ent.collision_group != group::DEBRIS || sweep.mask & contents::DEBRIS != 0;
                let collides = debris_hit && should_collide(sweep.group, ent.collision_group);
                (solid && collides && contents & sweep.mask != 0 && ent.player.as_ref().is_none_or(|p| p.alive)).then_some((h, contents))
            })
            .collect()
    };
    for (handle, contents) in candidates {
        let Some((pos, angles)) = host(lua).entities.transform(handle) else { continue };
        let Some((mins, maxs)) = host(lua).entities.get(handle).map(|ent| (ent.mins, ent.maxs)) else { continue };
        let found = if hull {
            // the box around it in the world, grown by the box swept
            let (lo, hi) = entity::rotated_bounds(mins, maxs, angles);
            through_box(sweep.start, sweep.end, pos + lo - sweep.maxs, pos + hi - sweep.mins)
        } else {
            // in its own space, where its box is upright
            let local = |p: Vector| entity::unrotated(p - pos, angles);
            through_box(local(sweep.start), local(sweep.end), mins, maxs).map(|h| BoxHit { normal: h.normal.rotated(angles), ..h })
        };
        let Some(found) = found else { continue };
        if found.fraction >= best.fraction && !found.start_solid {
            continue;
        }
        if !allowed(lua, sweep, handle) {
            continue;
        }
        best = Hit {
            fraction: found.fraction,
            end: sweep.start + (sweep.end - sweep.start) * found.fraction,
            normal: found.normal,
            start_solid: found.start_solid,
            all_solid: found.start_solid && found.left >= 1.0,
            fraction_left_solid: found.left,
            contents,
            entity: handle,
            texture: "**studio**".into(),
            surface_flags: 0,
            displacement: false,
        };
        if found.start_solid {
            break;
        }
    }
    best
}

/// The world's contents at a point (`CONTENTS_EMPTY` without a map).
pub(crate) fn point_contents(lua: &Lua, point: Vector) -> u32 {
    host(lua).world.collision.borrow().as_ref().map_or(contents::EMPTY, |c| c.point_contents(array(point)))
}

/// Whether a point is inside the map: within its bounds and not inside a brush. Without a map
/// everything is.
pub(crate) fn in_world(lua: &Lua, point: Vector) -> bool {
    let host = host(lua);
    let world = host.world.collision.borrow();
    let Some(collision) = world.as_ref() else { return true };
    let (lo, hi) = (vector(collision.mins), vector(collision.maxs));
    let inside = point.x >= lo.x && point.y >= lo.y && point.z >= lo.z && point.x <= hi.x && point.y <= hi.y && point.z <= hi.z;
    inside && collision.point_contents(array(point)) & contents::SOLID == 0
}

/// The `TraceResult` table of a hit.
pub(crate) fn result(lua: &Lua, sweep: &Sweep, hit: &Hit, out: Option<Table>) -> mlua::Result<Table> {
    let t = match out {
        Some(t) => t,
        None => lua.create_table()?,
    };
    let world = hit.entity == EntityHandle::WORLD;
    t.raw_set("Entity", entity::push(lua, hit.entity)?)?;
    t.raw_set("Fraction", hit.fraction)?;
    t.raw_set("FractionLeftSolid", hit.fraction_left_solid)?;
    t.raw_set("Hit", hit.hit())?;
    t.raw_set("HitBox", 0)?;
    t.raw_set("HitGroup", 0)?;
    t.raw_set("HitNoDraw", hit.surface_flags & surf::NODRAW != 0)?;
    t.raw_set("HitNonWorld", hit.hit() && !world)?;
    t.raw_set("HitNormal", entity::vector(lua, hit.normal)?)?;
    t.raw_set("HitPos", entity::vector(lua, hit.end)?)?;
    t.raw_set("HitSky", hit.surface_flags & surf::SKY != 0)?;
    t.raw_set("HitTexture", hit.texture.as_str())?;
    t.raw_set("HitWorld", hit.hit() && world)?;
    let surface = if !hit.hit() {
        None
    } else if world && !hit.texture.starts_with("**") {
        Some(crate::surfaceprops::of_material(lua, &hit.texture))
    } else {
        crate::physics::material_of(lua, hit.entity).map(|name| crate::surfaceprops::named(lua, &name))
    };
    let mat_type = match &surface {
        Some((_, prop)) => i64::from(prop.game_material),
        None if hit.hit() => MAT_DEFAULT,
        None => 0,
    };
    t.raw_set("MatType", mat_type)?;
    t.raw_set("Normal", entity::vector(lua, (sweep.end - sweep.start).normalized())?)?;
    t.raw_set("PhysicsBone", 0)?;
    t.raw_set("StartPos", entity::vector(lua, sweep.start)?)?;
    t.raw_set("SurfaceProps", surface.map_or(0, |(index, _)| index as i64))?;
    t.raw_set("StartSolid", hit.start_solid)?;
    t.raw_set("AllSolid", hit.all_solid)?;
    t.raw_set("SurfaceFlags", hit.surface_flags)?;
    t.raw_set("DispFlags", 0)?;
    t.raw_set("Contents", hit.contents)?;
    Ok(t)
}

/// A `filter` of a trace table: an entity, a list of entities and class names, a function.
fn filter_of(lua: &Lua, value: Value) -> mlua::Result<Filter> {
    Ok(match value {
        Value::Nil => Filter::None,
        Value::Function(f) => Filter::Function(f),
        Value::UserData(_) => Filter::Entities(entity::handle_of(Some(&value)).into_iter().collect()),
        Value::String(class) => Filter::Classes(vec![class.to_string_lossy()]),
        Value::Table(list) => {
            let (mut entities, mut classes) = (Vec::new(), Vec::new());
            for value in list.sequence_values::<Value>() {
                match value? {
                    Value::String(class) => classes.push(class.to_string_lossy()),
                    other => entities.extend(entity::handle_of(Some(&other))),
                }
            }
            if classes.is_empty() {
                Filter::Entities(entities)
            } else if entities.is_empty() {
                Filter::Classes(classes)
            } else {
                // both: the entities by class as well
                let host = host(lua);
                for handle in host.entities.handles() {
                    if host.entities.get(handle).is_some_and(|ent| classes.iter().any(|c| c.eq_ignore_ascii_case(&ent.class))) {
                        entities.push(handle);
                    }
                }
                Filter::Entities(entities)
            }
        }
        _ => Filter::None,
    })
}

fn vector_field(a: &Args, t: &Table, key: &str) -> mlua::Result<Option<Vector>> {
    match t.raw_get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::UserData(ud) => ud.borrow::<Vector>().map(|v| Some(*v)).map_err(|_| a.bad(1, "table with Vector fields")),
        _ => Err(crate::args::error(a.lua, format!("{key} is not a Vector"))),
    }
}

/// The trace a `TraceData` table asks for.
fn sweep_of(a: &Args, t: &Table, hull: bool) -> mlua::Result<Sweep> {
    let start = vector_field(a, t, "start")?.unwrap_or_default();
    let end = vector_field(a, t, "endpos")?.unwrap_or(start);
    let (mins, maxs) = if hull {
        (vector_field(a, t, "mins")?.unwrap_or_default(), vector_field(a, t, "maxs")?.unwrap_or_default())
    } else {
        (Vector::default(), Vector::default())
    };
    Ok(Sweep {
        start,
        end,
        mins,
        maxs,
        mask: t.raw_get::<Option<f64>>("mask")?.map_or(mask::SOLID, |m| m as i64 as u32),
        filter: filter_of(a.lua, t.raw_get("filter")?)?,
        whitelist: t.raw_get::<Option<bool>>("whitelist")?.unwrap_or(false),
        ignore_world: t.raw_get::<Option<bool>>("ignoreworld")?.unwrap_or(false),
        group: t.raw_get::<Option<f64>>("collisiongroup")?.map_or(group::NONE, |g| g as i64),
    })
}

/// `util.TraceLine` and the like: the result, or nothing when it went into `output`.
fn run(a: &Args, sweep: Sweep, t: &Table) -> mlua::Result<Value> {
    let hit = trace(a.lua, &sweep);
    match t.raw_get::<Option<Table>>("output")? {
        Some(out) => {
            result(a.lua, &sweep, &hit, Some(out))?;
            Ok(Value::Nil)
        }
        None => Ok(Value::Table(result(a.lua, &sweep, &hit, None)?)),
    }
}

/// The eye trace of a player (`GetEyeTrace`): from the eyes along the aim, the player left out.
pub(crate) fn eye_trace(lua: &Lua, player: EntityHandle) -> mlua::Result<Value> {
    let Some((eye, aim)) = crate::player::eye(lua, player) else { return Ok(Value::Nil) };
    let sweep = Sweep::line(eye, eye + aim * EYE_TRACE_LENGTH, Filter::Entities(vec![player]));
    let hit = trace(lua, &sweep);
    Ok(Value::Table(result(lua, &sweep, &hit, None)?))
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let util: Table = lua.globals().get("util")?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        util.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("TraceLine", |a| {
        let t = a.table(1)?;
        run(a, sweep_of(a, &t, false)?, &t)
    })?;
    def("TraceHull", |a| {
        let t = a.table(1)?;
        run(a, sweep_of(a, &t, true)?, &t)
    })?;
    // the entity's box, unturned, swept
    def("TraceEntity", |a| {
        let t = a.table(1)?;
        let handle = entity::this(a, 2)?;
        let mut sweep = sweep_of(a, &t, false)?;
        if let Some(ent) = host(a.lua).entities.get(handle) {
            (sweep.mins, sweep.maxs) = (ent.mins, ent.maxs);
        }
        run(a, sweep, &t)
    })?;
    def("PointContents", |a| Ok(Value::Integer(point_contents(a.lua, a.userdata::<Vector>(1, "Vector")?).into())))?;
    def("IsInWorld", |a| Ok(Value::Boolean(in_world(a.lua, a.userdata::<Vector>(1, "Vector")?))))?;
    Ok(())
}
