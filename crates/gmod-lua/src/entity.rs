//! Entities as Lua sees them: the engine's list of them, the userdata scripts hold, the named
//! metatables of the engine's types, and the methods of `Entity` that GMod implements in C++.
//!
//! Every entity has an index (0 is the world, the next ones are kept for players) and a
//! serial number: a handle to an entity that was removed stays NULL even when its index is
//! used again. Lua always gets the same userdata for an entity, so entities work as table
//! keys, and each has a table of its own (`GetTable`) where `ent.foo = 1` goes. What
//! `ent.foo` finds is up to the named metatable's `__index` - the game's Lua sets it
//! (`includes/extensions/entity.lua`): the metatable first, then the entity's table.
//!
//! Creating, spawning and removing entities, the `ents` library and the entities of a map
//! are in [`crate::ents`].

use std::cell::{Cell, Ref, RefCell};

use mlua::{AnyUserData, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::{self, Args};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};
use crate::{Realm, host};

/// Entity indices the engine has: edicts, as GMod's `MAX_EDICTS`.
pub const MAX_EDICTS: u32 = 8192;

/// What an entity is to Lua: which named metatable it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Entity,
    Player,
    Weapon,
    Npc,
    Vehicle,
    NextBot,
}

impl Kind {
    pub fn meta_name(self) -> &'static str {
        match self {
            Kind::Entity => "Entity",
            Kind::Player => "Player",
            Kind::Weapon => "Weapon",
            Kind::Npc => "NPC",
            Kind::Vehicle => "Vehicle",
            Kind::NextBot => "NextBot",
        }
    }
}

/// A handle to an entity: its index and serial number, or none for `NULL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityHandle {
    pub index: Option<u32>,
    pub serial: u32,
    pub kind: Kind,
}

impl EntityHandle {
    pub const NULL: EntityHandle = EntityHandle { index: None, serial: 0, kind: Kind::Entity };
    pub const WORLD: EntityHandle = EntityHandle { index: Some(0), serial: 0, kind: Kind::Entity };
}

/// One entity, as the engine keeps it. Position and angles are relative to the parent, if
/// there is one.
#[derive(Debug, Clone)]
pub struct Ent {
    pub class: String,
    pub kind: Kind,
    /// Its table comes from `scripted_ents` or `weapons`: a SENT or a SWEP.
    pub scripted: bool,
    pub creation_id: u32,
    pub created: f64,
    pub map_creation_id: Option<i32>,
    pub local_pos: Vector,
    pub local_angles: Angle,
    pub velocity: Vector,
    pub model: String,
    pub name: String,
    /// Key-values as the map or `SetKeyValue` gave them, a key once.
    pub keyvalues: Vec<(String, String)>,
    pub spawned: bool,
    /// `Remove` was called: it goes at the end of the frame.
    pub removed: bool,
    pub owner: EntityHandle,
    pub parent: EntityHandle,
    pub skin: i64,
    pub color: [u8; 4],
    pub material: String,
    pub render_mode: i64,
    pub health: i64,
    pub max_health: i64,
    pub spawn_flags: i64,
    pub flags: i64,
    pub engine_flags: i64,
    pub effects: i64,
    pub move_type: i64,
    pub solid: i64,
    pub solid_flags: i64,
    pub collision_group: i64,
    pub use_type: i64,
    pub trigger: bool,
    pub custom_collisions: bool,
    /// Kept across map loads by sandbox (`SetPersistent`).
    pub persistent: bool,
    pub transmit_with_parent: bool,
    pub mins: Vector,
    pub maxs: Vector,
    pub no_draw: bool,
    pub draw_shadow: bool,
    pub model_scale: f32,
    /// Body groups set: number and value.
    pub body_groups: Vec<(i64, i64)>,
    /// When a SENT's `Think` runs next, and the time `NextThink` asked for.
    pub next_think: f64,
    pub think_request: Option<f64>,
    /// What only players have.
    pub player: Option<Box<crate::player::PlayerState>>,
    /// What only weapons have.
    pub weapon: Option<crate::weapon::WeaponState>,
    /// Its body, when it has physics ([`crate::physics`]).
    pub physics: Option<gmod_physics::BodyId>,
    /// What a constraint holds together, as `SetPhysConstraintObjects` gave it: the owners of
    /// the two bodies ([`crate::constraint`]).
    pub constrained: Option<[EntityHandle; 2]>,
    /// The joint a constraint made.
    pub joint: Option<gmod_physics::JointId>,
    /// Entities given for keys (`SetEntity`): a rope's ends.
    pub links: Vec<(String, EntityHandle)>,
    /// Whether it shows the effect of being spawned (`SetSpawnEffect`).
    pub spawn_effect: bool,
    /// How it is drawn changing (`kRenderFx`): pulsing, fading, a hologram.
    pub render_fx: i64,
    /// Materials in place of the model's own, by slot (`SetSubMaterial`).
    pub sub_materials: Vec<(i64, String)>,
    /// How much its flexes move (`SetFlexScale`), and how far each is moved.
    pub flex_scale: f32,
    pub flex_weights: Vec<(i64, f32)>,
    /// Bones moved, turned and scaled from where the model has them (`ManipulateBone*`).
    pub bone_manipulations: Vec<BoneManipulation>,
}

/// How a bone is moved from where its model has it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoneManipulation {
    pub bone: i64,
    pub position: Vector,
    pub angles: Angle,
    pub scale: Vector,
}

impl BoneManipulation {
    fn new(bone: i64) -> BoneManipulation {
        BoneManipulation { bone, position: Vector::default(), angles: Angle::default(), scale: Vector::new(1.0, 1.0, 1.0) }
    }

    fn is_none(&self) -> bool {
        *self == BoneManipulation::new(self.bone)
    }
}

impl Ent {
    pub fn new(class: &str, kind: Kind, creation_id: u32, created: f64) -> Ent {
        Ent {
            class: class.to_string(),
            kind,
            scripted: false,
            creation_id,
            created,
            map_creation_id: None,
            local_pos: Vector::default(),
            local_angles: Angle::default(),
            velocity: Vector::default(),
            model: String::new(),
            name: String::new(),
            keyvalues: Vec::new(),
            spawned: false,
            removed: false,
            owner: EntityHandle::NULL,
            parent: EntityHandle::NULL,
            skin: 0,
            color: [255; 4],
            material: String::new(),
            render_mode: 0,
            health: 0,
            max_health: 0,
            spawn_flags: 0,
            flags: 0,
            engine_flags: 0,
            effects: 0,
            move_type: 0,
            solid: 0,
            solid_flags: 0,
            collision_group: 0,
            use_type: 0,
            trigger: false,
            custom_collisions: false,
            persistent: false,
            transmit_with_parent: false,
            mins: Vector::default(),
            maxs: Vector::default(),
            no_draw: false,
            draw_shadow: true,
            model_scale: 1.0,
            body_groups: Vec::new(),
            next_think: 0.0,
            think_request: None,
            player: None,
            weapon: None,
            physics: None,
            constrained: None,
            joint: None,
            links: Vec::new(),
            spawn_effect: false,
            render_fx: 0,
            sub_materials: Vec::new(),
            flex_scale: 1.0,
            flex_weights: Vec::new(),
            bone_manipulations: Vec::new(),
        }
    }

    pub fn keyvalue(&self, key: &str) -> Option<&str> {
        self.keyvalues.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

struct Slot {
    serial: u32,
    ent: Option<Ent>,
}

/// Every entity there is, by index.
#[derive(Default)]
pub(crate) struct Entities {
    slots: RefCell<Vec<Slot>>,
    next_creation_id: Cell<u32>,
}

impl Entities {
    /// The entity behind a handle, if it is still there.
    pub(crate) fn get(&self, handle: EntityHandle) -> Option<Ref<'_, Ent>> {
        let index = handle.index? as usize;
        Ref::filter_map(self.slots.borrow(), |slots| {
            slots.get(index).filter(|slot| slot.serial == handle.serial).and_then(|slot| slot.ent.as_ref())
        })
        .ok()
    }

    pub(crate) fn with_mut<R>(&self, handle: EntityHandle, f: impl FnOnce(&mut Ent) -> R) -> Option<R> {
        let index = handle.index? as usize;
        let mut slots = self.slots.borrow_mut();
        let slot = slots.get_mut(index).filter(|slot| slot.serial == handle.serial)?;
        slot.ent.as_mut().map(f)
    }

    pub(crate) fn is_valid(&self, handle: EntityHandle) -> bool {
        self.get(handle).is_some()
    }

    /// The handle of the entity at an index.
    pub(crate) fn at(&self, index: u32) -> Option<EntityHandle> {
        let slots = self.slots.borrow();
        let slot = slots.get(index as usize)?;
        slot.ent.as_ref().map(|ent| EntityHandle { index: Some(index), serial: slot.serial, kind: ent.kind })
    }

    /// Every entity, in index order.
    pub(crate) fn handles(&self) -> Vec<EntityHandle> {
        let slots = self.slots.borrow();
        slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                slot.ent.as_ref().map(|ent| EntityHandle { index: Some(i as u32), serial: slot.serial, kind: ent.kind })
            })
            .collect()
    }

    pub(crate) fn next_creation_id(&self) -> u32 {
        let id = self.next_creation_id.get();
        self.next_creation_id.set(id + 1);
        id
    }

    /// Puts an entity at the lowest free index from `first`; none when every edict is used.
    pub(crate) fn insert(&self, ent: Ent, first: u32) -> Option<EntityHandle> {
        let mut slots = self.slots.borrow_mut();
        let kind = ent.kind;
        let index = (first as usize..MAX_EDICTS as usize).find(|&i| slots.get(i).is_none_or(|slot| slot.ent.is_none()))?;
        while slots.len() <= index {
            slots.push(Slot { serial: 0, ent: None });
        }
        let slot = &mut slots[index];
        slot.ent = Some(ent);
        Some(EntityHandle { index: Some(index as u32), serial: slot.serial, kind })
    }

    /// Puts an entity at an index under a serial number given: a client's copy of the
    /// server's entity there. What was at the index is gone.
    pub(crate) fn put(&self, index: u32, serial: u32, ent: Ent) -> EntityHandle {
        let mut slots = self.slots.borrow_mut();
        while slots.len() <= index as usize {
            slots.push(Slot { serial: 0, ent: None });
        }
        let kind = ent.kind;
        slots[index as usize] = Slot { serial, ent: Some(ent) };
        EntityHandle { index: Some(index), serial, kind }
    }

    /// Frees an index: the handles to what was there become NULL.
    pub(crate) fn free(&self, handle: EntityHandle) {
        let Some(index) = handle.index else { return };
        let mut slots = self.slots.borrow_mut();
        if let Some(slot) = slots.get_mut(index as usize).filter(|slot| slot.serial == handle.serial) {
            slot.ent = None;
            slot.serial = slot.serial.wrapping_add(1);
        }
    }

    /// Position and angles in the world, through the parents.
    pub(crate) fn transform(&self, handle: EntityHandle) -> Option<(Vector, Angle)> {
        let ent = self.get(handle)?;
        let (pos, angles, parent) = (ent.local_pos, ent.local_angles, ent.parent);
        drop(ent);
        Some(match self.parent_transform(parent, 0) {
            Some((parent_pos, parent_angles)) => (parent_pos + pos.rotated(parent_angles), compose(parent_angles, angles)),
            None => (pos, angles),
        })
    }

    fn parent_transform(&self, parent: EntityHandle, depth: usize) -> Option<(Vector, Angle)> {
        if depth > 32 {
            return None;
        }
        let ent = self.get(parent)?;
        let (pos, angles, grandparent) = (ent.local_pos, ent.local_angles, ent.parent);
        drop(ent);
        Some(match self.parent_transform(grandparent, depth + 1) {
            Some((p, a)) => (p + pos.rotated(a), compose(a, angles)),
            None => (pos, angles),
        })
    }

    /// The transform of an entity's parent, which its local position is relative to.
    pub(crate) fn parent_of(&self, handle: EntityHandle) -> Option<(Vector, Angle)> {
        let parent = self.get(handle)?.parent;
        self.transform(parent)
    }

    /// The box around an entity in the world: its collision box turned by its angles.
    pub(crate) fn world_bounds(&self, handle: EntityHandle) -> Option<(Vector, Angle, Vector, Vector)> {
        let (pos, angles) = self.transform(handle)?;
        let ent = self.get(handle)?;
        let (lo, hi) = rotated_bounds(ent.mins, ent.maxs, angles);
        Some((pos, angles, pos + lo, pos + hi))
    }
}

/// The angles of a rotation by `local` inside a space turned by `outer`.
pub fn compose(outer: Angle, local: Angle) -> Angle {
    let (forward, _, up) = local.vectors();
    forward.rotated(outer).angle_with_up(up.rotated(outer))
}

/// A world direction in the space of an angle: the inverse of [`Vector::rotated`].
pub fn unrotated(v: Vector, angle: Angle) -> Vector {
    let (forward, right, up) = angle.vectors();
    Vector::new(v.dot(forward), -v.dot(right), v.dot(up))
}

/// The angles of `world` as seen from a space turned by `outer`: the inverse of [`compose`].
pub fn decompose(outer: Angle, world: Angle) -> Angle {
    let (forward, _, up) = world.vectors();
    unrotated(forward, outer).angle_with_up(unrotated(up, outer))
}

/// `AlignAngles`: `angles` turned as `from` would have to turn to be `to`.
pub fn align(angles: Angle, from: Angle, to: Angle) -> Angle {
    compose(to, decompose(from, angles))
}

/// The axis-aligned box around a box turned by `angles`.
pub fn rotated_bounds(mins: Vector, maxs: Vector, angles: Angle) -> (Vector, Vector) {
    let mut lo = Vector::new(f32::MAX, f32::MAX, f32::MAX);
    let mut hi = Vector::new(f32::MIN, f32::MIN, f32::MIN);
    for i in 0..8 {
        let corner = Vector::new(
            if i & 1 == 0 { mins.x } else { maxs.x },
            if i & 2 == 0 { mins.y } else { maxs.y },
            if i & 4 == 0 { mins.z } else { maxs.z },
        )
        .rotated(angles);
        lo = Vector::new(lo.x.min(corner.x), lo.y.min(corner.y), lo.z.min(corner.z));
        hi = Vector::new(hi.x.max(corner.x), hi.y.max(corner.y), hi.z.max(corner.z));
    }
    (lo, hi)
}

const CACHE: &str = "gmod-lua.entities";
/// `SOLID_VPHYSICS`: the entity collides as its physics body.
const SOLID_VPHYSICS: i64 = 6;
const NULL_ENTITY: &str = "gmod-lua.null-entity";

/// The engine's types that are not entities: name, type ID, and whether only clients have it.
const TYPES: &[(&str, i32, bool)] = {
    use type_id::*;
    &[
        ("PhysObj", PHYSOBJ, false),
        ("ISave", SAVE, false),
        ("IRestore", RESTORE, false),
        ("CTakeDamageInfo", DAMAGEINFO, false),
        ("CEffectData", EFFECTDATA, false),
        ("CMoveData", MOVEDATA, false),
        ("CRecipientFilter", RECIPIENTFILTER, false),
        ("CUserCmd", USERCMD, false),
        ("IMaterial", MATERIAL, false),
        ("Panel", PANEL, true),
        ("CLuaParticle", PARTICLE, true),
        ("CLuaEmitter", PARTICLEEMITTER, true),
        ("ITexture", TEXTURE, false),
        ("bf_read", USERMSG, true),
        ("ConVar", CONVAR, false),
        ("IMesh", IMESH, true),
        ("VMatrix", MATRIX, false),
        ("CSoundPatch", SOUND, false),
        ("IVideoWriter", VIDEO, true),
        ("File", FILE, false),
        ("CLuaLocomotion", LOCOMOTION, false),
        ("PathFollower", PATH, false),
        ("CNavArea", NAVAREA, false),
        ("IGModAudioChannel", SOUNDHANDLE, true),
        ("CNavLadder", NAVLADDER, false),
        ("CNewParticleEffect", PARTICLESYSTEM, false),
        ("ProjectedTexture", PROJECTEDTEXTURE, true),
        ("PhysCollide", PHYSCOLLIDE, false),
        ("SurfaceInfo", SURFACEINFO, false),
    ]
};

/// Kinds of entity, each with a metatable whose `MetaBaseClass` is `Entity`'s.
const ENTITY_KINDS: &[(&str, bool)] =
    &[("Player", false), ("Weapon", false), ("NPC", false), ("Vehicle", false), ("NextBot", false), ("CSEnt", true)];

/// The physics object of an entity: valid while the entity has a body ([`crate::physics`]).
/// That of an entity without one holds `NULL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysObjHandle(pub EntityHandle);

impl UserData for PhysObjHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "PhysObj");
        fields.add_meta_field("MetaID", type_id::PHYSOBJ);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "PhysObj", key));
        methods.add_meta_function("__tostring", |lua, ud: AnyUserData| {
            let phys = *ud.borrow::<PhysObjHandle>()?;
            if !crate::physics::valid(lua, phys) {
                return Ok("[NULL PhysObj]".to_string());
            }
            let class = host(lua).entities.get(phys.0).map(|ent| ent.class.clone()).unwrap_or_default();
            Ok(format!("PhysObject [Entity [{}][{class}]]", phys.0.index.unwrap_or(0)))
        });
        methods.add_meta_function("__eq", |_, (a, b): (AnyUserData, AnyUserData)| {
            Ok(matches!((a.borrow::<PhysObjHandle>(), b.borrow::<PhysObjHandle>()), (Ok(a), Ok(b)) if *a == *b))
        });
    }
}

impl UserData for EntityHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "Entity");
        fields.add_meta_field("MetaID", type_id::ENTITY);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| index(lua, ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| -> mlua::Result<()> {
            let handle = *ud.borrow::<EntityHandle>()?;
            if !host(lua).entities.is_valid(handle) {
                return Err(args::error(lua, "Tried to use a NULL entity!"));
            }
            match table_of(&ud)? {
                Some(table) => table.set(key, value),
                None => Ok(()),
            }
        });
        methods.add_meta_function("__eq", |_, (a, b): (AnyUserData, AnyUserData)| {
            Ok(matches!((a.borrow::<EntityHandle>(), b.borrow::<EntityHandle>()), (Ok(a), Ok(b)) if *a == *b))
        });
        methods.add_meta_function("__tostring", |lua, ud: AnyUserData| {
            let handle = *ud.borrow::<EntityHandle>()?;
            Ok(match (handle.index, host(lua).entities.get(handle)) {
                (Some(index), Some(ent)) => format!("{} [{index}][{}]", handle.kind.meta_name(), ent.class),
                _ => "[NULL Entity]".to_string(),
            })
        });
    }
}

/// `ent.key`: what the named metatable's `__index` says - a function the game's Lua set, or
/// else the metatable, its base classes and the entity's table, in that order.
fn index(lua: &Lua, ud: AnyUserData, key: Value) -> mlua::Result<Value> {
    let kind = ud.borrow::<EntityHandle>()?.kind;
    let Some(meta) = meta::named(lua, kind.meta_name())? else { return Ok(Value::Nil) };
    if let Value::Function(f) = meta.raw_get::<Value>("__index")? {
        return f.call(MultiValue::from_iter([Value::UserData(ud), key]));
    }
    let mut table = Some(meta);
    for _ in 0..8 {
        let Some(t) = table else { break };
        let value = t.raw_get::<Value>(key.clone())?;
        if !value.is_nil() {
            return Ok(value);
        }
        table = t.raw_get::<Option<Table>>("MetaBaseClass")?;
    }
    match table_of(&ud)? {
        Some(t) if host(lua).entities.is_valid(*ud.borrow::<EntityHandle>()?) => t.get(key),
        _ => Ok(Value::Nil),
    }
}

/// The entity's own table (`GetTable`).
pub(crate) fn table_of(ud: &AnyUserData) -> mlua::Result<Option<Table>> {
    ud.named_user_value::<Option<Table>>("table")
}

/// A table the engine keeps with an entity (its networked variables), made when first used.
pub(crate) fn store_of(lua: &Lua, ud: &AnyUserData, name: &str) -> mlua::Result<Table> {
    if let Some(table) = ud.named_user_value::<Option<Table>>(name)? {
        return Ok(table);
    }
    let table = lua.create_table()?;
    ud.set_named_user_value(name, &table)?;
    Ok(table)
}

/// The userdata of an entity: the same one every time it is asked for, or `NULL` when the
/// entity is not there.
pub(crate) fn push(lua: &Lua, handle: EntityHandle) -> mlua::Result<Value> {
    if let Some(index) = handle.index.filter(|_| host(lua).entities.is_valid(handle)) {
        let cache: Table = lua.named_registry_value(CACHE)?;
        if let Some(ud) = cache.raw_get::<Option<AnyUserData>>(index)?
            && *ud.borrow::<EntityHandle>()? == handle
        {
            return Ok(Value::UserData(ud));
        }
        let ud = lua.create_userdata(handle)?;
        ud.set_named_user_value("table", lua.create_table()?)?;
        cache.raw_set(index, &ud)?;
        return Ok(Value::UserData(ud));
    }
    lua.named_registry_value(NULL_ENTITY)
}

/// The userdata of the entity at an index, or `NULL`.
pub(crate) fn push_index(lua: &Lua, index: u32) -> mlua::Result<Value> {
    let handle = host(lua).entities.at(index).unwrap_or(EntityHandle::NULL);
    push(lua, handle)
}

/// Forgets the userdata of an entity that is gone.
pub(crate) fn forget(lua: &Lua, handle: EntityHandle) -> mlua::Result<()> {
    let Some(index) = handle.index else { return Ok(()) };
    let cache: Table = lua.named_registry_value(CACHE)?;
    if let Some(ud) = cache.raw_get::<Option<AnyUserData>>(index)?
        && *ud.borrow::<EntityHandle>()? == handle
    {
        cache.raw_set(index, Value::Nil)?;
    }
    Ok(())
}

/// The entity a value is, valid or not.
pub(crate) fn handle_of(value: Option<&Value>) -> Option<EntityHandle> {
    match value {
        Some(Value::UserData(ud)) => ud.borrow::<EntityHandle>().ok().map(|h| *h),
        _ => None,
    }
}

/// The entity an argument names, which must be valid.
pub(crate) fn this(a: &Args, position: usize) -> mlua::Result<EntityHandle> {
    let handle = a.userdata::<EntityHandle>(position, "Entity")?;
    if host(a.lua).entities.is_valid(handle) { Ok(handle) } else { Err(args::error(a.lua, "Tried to use a NULL entity!")) }
}

/// An entity argument that may be NULL (or nil): `None` then.
fn opt_entity(a: &Args, position: usize) -> mlua::Result<Option<EntityHandle>> {
    match a.get(position) {
        None | Some(Value::Nil) => Ok(None),
        _ => {
            let handle = a.userdata::<EntityHandle>(position, "Entity")?;
            Ok(host(a.lua).entities.is_valid(handle).then_some(handle))
        }
    }
}

fn read<R>(a: &Args, f: impl FnOnce(&Ent) -> R) -> mlua::Result<R> {
    let handle = this(a, 1)?;
    let host = host(a.lua);
    let ent = host.entities.get(handle).expect("checked above");
    Ok(f(&ent))
}

fn write(a: &Args, f: impl FnOnce(&mut Ent)) -> mlua::Result<Value> {
    let handle = this(a, 1)?;
    host(a.lua).entities.with_mut(handle, f);
    Ok(Value::Nil)
}

/// The points of a convex shape, as `PhysicsInitConvex` takes them: a sequence of Vectors.
fn convex(table: &Table) -> mlua::Result<Vec<[f32; 3]>> {
    table
        .sequence_values::<AnyUserData>()
        .map(|point| {
            let point = point?;
            let v = point.borrow::<Vector>()?;
            Ok([v.x, v.y, v.z])
        })
        .collect()
}

pub(crate) fn vector(lua: &Lua, v: Vector) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(v)?))
}

pub(crate) fn angle(lua: &Lua, a: Angle) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(a)?))
}

fn integer(n: i64) -> mlua::Result<Value> {
    Ok(Value::Integer(n))
}

fn string(lua: &Lua, text: &str) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(text)?))
}

/// The world transform of the entity in argument 1.
fn transform(a: &Args) -> mlua::Result<(Vector, Angle)> {
    let handle = this(a, 1)?;
    Ok(host(a.lua).entities.transform(handle).expect("checked above"))
}

/// Sets where an entity is in the world, kept relative to its parent.
pub(crate) fn set_world_pos(lua: &Lua, handle: EntityHandle, pos: Vector) {
    let entities = &host(lua).entities;
    let local = match entities.parent_of(handle) {
        Some((parent_pos, parent_angles)) => unrotated(pos - parent_pos, parent_angles),
        None => pos,
    };
    entities.with_mut(handle, |ent| ent.local_pos = local);
    crate::physics::moved(lua, handle);
}

pub(crate) fn set_world_angles(lua: &Lua, handle: EntityHandle, angles: Angle) {
    let entities = &host(lua).entities;
    let local = match entities.parent_of(handle) {
        Some((_, parent_angles)) => decompose(parent_angles, angles),
        None => angles,
    };
    entities.with_mut(handle, |ent| ent.local_angles = local);
    crate::physics::moved(lua, handle);
}

/// Sets a model and the collision box it comes with: the hull of a studio model, the bounds
/// of a brush model of the map (`*3`). A player keeps its hull.
pub(crate) fn set_model(lua: &Lua, handle: EntityHandle, model: &str) {
    let bounds = crate::ents::model_bounds(lua, model);
    host(lua).entities.with_mut(handle, |ent| {
        ent.model = model.to_string();
        // a player's box is its hull, whatever the model
        if let Some((mins, maxs)) = bounds.filter(|_| ent.player.is_none()) {
            ent.mins = mins;
            ent.maxs = maxs;
        }
    });
}

/// `IsPlayer` and the like: whether an entity is of a kind.
fn is_kind(a: &Args, kind: Kind) -> mlua::Result<Value> {
    Ok(Value::Boolean(read(a, |ent| ent.kind == kind)?))
}

fn color_part(a: &Args, position: usize) -> mlua::Result<u8> {
    Ok(a.opt_number(position)?.unwrap_or(255.0).clamp(0.0, 255.0) as u8)
}

pub(crate) fn install(lua: &Lua, realm: Realm) -> mlua::Result<()> {
    lua.set_named_registry_value(CACHE, lua.create_table()?)?;
    let null = lua.create_userdata(EntityHandle::NULL)?;
    lua.set_named_registry_value(NULL_ENTITY, &null)?;
    lua.globals().raw_set("NULL", null)?;

    let client = realm != Realm::Server;
    for (name, id, client_only) in TYPES {
        if client || !client_only {
            meta::meta_table(lua, name, *id)?;
        }
    }
    let entity = meta::meta_table(lua, "Entity", type_id::ENTITY)?;
    for (name, client_only) in ENTITY_KINDS {
        if client || !client_only {
            let kind = meta::meta_table(lua, name, type_id::ENTITY)?;
            kind.raw_set("MetaBaseClass", &entity)?;
        }
    }

    // the world is there from the start, at index 0
    if realm != Realm::Menu {
        let world = Ent::new("worldspawn", Kind::Entity, host(lua).entities.next_creation_id(), 0.0);
        host(lua).entities.insert(world, 0);
    }

    crate::physics::install(lua)?;

    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        entity.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    install_methods(&def)?;
    let many = |name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>| -> mlua::Result<()> {
        entity.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    install_many(&many)?;
    Ok(())
}

pub(crate) type Def<'a> = dyn Fn(&'static str, fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> + 'a;
/// Defines a method that gives more than one value.
pub(crate) type DefMany<'a> = dyn Fn(&'static str, fn(&Args) -> mlua::Result<MultiValue>) -> mlua::Result<()> + 'a;

/// The methods that give more than one value.
fn install_many(def: &DefMany) -> mlua::Result<()> {
    def("GetCollisionBounds", |a| {
        let (mins, maxs) = read(a, |ent| (ent.mins, ent.maxs))?;
        Ok(MultiValue::from_iter([vector(a.lua, mins)?, vector(a.lua, maxs)?]))
    })?;
    def("GetModelBounds", |a| {
        let model = read(a, |ent| ent.model.clone())?;
        let (mins, maxs) = crate::ents::model_bounds(a.lua, &model).unwrap_or_default();
        Ok(MultiValue::from_iter([vector(a.lua, mins)?, vector(a.lua, maxs)?]))
    })?;
    def("GetColor4Part", |a| {
        let color = read(a, |ent| ent.color)?;
        Ok(color.into_iter().map(|part| Value::Integer(part.into())).collect())
    })?;
    crate::constraint::install_many(def)
}

fn install_methods(def: &Def) -> mlua::Result<()> {
    // what it is
    def("IsValid", |a| {
        let valid = handle_of(a.get(1)).is_some_and(|h| host(a.lua).entities.is_valid(h));
        Ok(Value::Boolean(valid))
    })?;
    def("EntIndex", |a| {
        let handle = a.userdata::<EntityHandle>(1, "Entity")?;
        let index = if host(a.lua).entities.is_valid(handle) { handle.index.unwrap_or(0) } else { 0 };
        integer(index.into())
    })?;
    def("IsWorld", |a| {
        let handle = a.userdata::<EntityHandle>(1, "Entity")?;
        Ok(Value::Boolean(handle.index == Some(0) && host(a.lua).entities.is_valid(handle)))
    })?;
    def("GetClass", |a| {
        let class = read(a, |ent| ent.class.clone())?;
        string(a.lua, &class)
    })?;
    def("GetTable", |a| {
        let handle = a.userdata::<EntityHandle>(1, "Entity")?;
        if !host(a.lua).entities.is_valid(handle) {
            return Ok(Value::Nil);
        }
        match a.get(1) {
            Some(Value::UserData(ud)) => Ok(table_of(ud)?.map_or(Value::Nil, Value::Table)),
            _ => Ok(Value::Nil),
        }
    })?;
    def("SetTable", |a| {
        let handle = this(a, 1)?;
        let table = a.table(2)?;
        if let Value::UserData(ud) = push(a.lua, handle)? {
            ud.set_named_user_value("table", table)?;
        }
        Ok(Value::Nil)
    })?;
    def("IsPlayer", |a| is_kind(a, Kind::Player))?;
    def("IsNPC", |a| is_kind(a, Kind::Npc))?;
    def("IsWeapon", |a| is_kind(a, Kind::Weapon))?;
    def("IsVehicle", |a| is_kind(a, Kind::Vehicle))?;
    def("IsNextBot", |a| is_kind(a, Kind::NextBot))?;
    def("IsRagdoll", |a| Ok(Value::Boolean(read(a, |ent| ent.class.eq_ignore_ascii_case("prop_ragdoll"))?)))?;
    def("IsScripted", |a| Ok(Value::Boolean(read(a, |ent| ent.scripted)?)))?;
    def("CreatedByMap", |a| Ok(Value::Boolean(read(a, |ent| ent.map_creation_id.is_some())?)))?;
    def("MapCreationID", |a| integer(read(a, |ent| ent.map_creation_id.unwrap_or(-1))?.into()))?;
    def("GetCreationID", |a| integer(read(a, |ent| ent.creation_id)?.into()))?;
    def("GetCreationTime", |a| Ok(Value::Number(read(a, |ent| ent.created)?)))?;
    def("IsMarkedForDeletion", |a| Ok(Value::Boolean(read(a, |ent| ent.removed)?)))?;

    // where it is
    def("GetPos", |a| vector(a.lua, transform(a)?.0))?;
    def("GetAngles", |a| angle(a.lua, transform(a)?.1))?;
    def("SetPos", |a| {
        let handle = this(a, 1)?;
        set_world_pos(a.lua, handle, a.userdata::<Vector>(2, "Vector")?);
        Ok(Value::Nil)
    })?;
    // down to what is below, up to 256 units, as the engine's UTIL_DropToFloor
    def("DropToFloor", |a| {
        let handle = this(a, 1)?;
        let pos = transform(a)?.0;
        let (mins, maxs) = read(a, |ent| (ent.mins, ent.maxs))?;
        let sweep = crate::trace::Sweep {
            mins,
            maxs,
            mask: gmod_bsp::mask::SOLID,
            ..crate::trace::Sweep::line(pos, pos - Vector::new(0.0, 0.0, 256.0), crate::trace::Filter::Entities(vec![handle]))
        };
        let hit = crate::trace::trace(a.lua, &sweep);
        if !hit.all_solid && hit.fraction < 1.0 {
            set_world_pos(a.lua, handle, hit.end);
        }
        Ok(Value::Nil)
    })?;
    def("SetAngles", |a| {
        let handle = this(a, 1)?;
        set_world_angles(a.lua, handle, a.userdata::<Angle>(2, "Angle")?);
        Ok(Value::Nil)
    })?;
    def("GetLocalPos", |a| vector(a.lua, read(a, |ent| ent.local_pos)?))?;
    def("GetLocalAngles", |a| angle(a.lua, read(a, |ent| ent.local_angles)?))?;
    def("SetLocalPos", |a| {
        let pos = a.userdata::<Vector>(2, "Vector")?;
        write(a, |ent| ent.local_pos = pos)?;
        crate::physics::moved(a.lua, this(a, 1)?);
        Ok(Value::Nil)
    })?;
    def("SetLocalAngles", |a| {
        let angles = a.userdata::<Angle>(2, "Angle")?;
        write(a, |ent| ent.local_angles = angles)?;
        crate::physics::moved(a.lua, this(a, 1)?);
        Ok(Value::Nil)
    })?;
    def("GetForward", |a| vector(a.lua, transform(a)?.1.vectors().0))?;
    def("GetRight", |a| vector(a.lua, transform(a)?.1.vectors().1))?;
    def("GetUp", |a| vector(a.lua, transform(a)?.1.vectors().2))?;
    def("EyePos", |a| {
        let handle = this(a, 1)?;
        match crate::player::eye(a.lua, handle) {
            Some((eye, _)) => vector(a.lua, eye),
            None => vector(a.lua, transform(a)?.0),
        }
    })?;
    def("EyeAngles", |a| {
        let handle = this(a, 1)?;
        match crate::player::eye_angles(a.lua, handle) {
            Some(angles) => angle(a.lua, angles),
            None => angle(a.lua, transform(a)?.1),
        }
    })?;
    def("LocalToWorld", |a| {
        let (pos, angles) = transform(a)?;
        vector(a.lua, pos + a.userdata::<Vector>(2, "Vector")?.rotated(angles))
    })?;
    def("WorldToLocal", |a| {
        let (pos, angles) = transform(a)?;
        vector(a.lua, unrotated(a.userdata::<Vector>(2, "Vector")? - pos, angles))
    })?;
    def("LocalToWorldAngles", |a| {
        let (_, angles) = transform(a)?;
        angle(a.lua, compose(angles, a.userdata::<Angle>(2, "Angle")?))
    })?;
    def("WorldToLocalAngles", |a| {
        let (_, angles) = transform(a)?;
        angle(a.lua, decompose(angles, a.userdata::<Angle>(2, "Angle")?))
    })?;
    def("AlignAngles", |a| {
        let (_, angles) = transform(a)?;
        angle(a.lua, align(angles, a.userdata::<Angle>(2, "Angle")?, a.userdata::<Angle>(3, "Angle")?))
    })?;
    def("GetVelocity", |a| vector(a.lua, read(a, |ent| ent.velocity)?))?;
    def("GetAbsVelocity", |a| vector(a.lua, read(a, |ent| ent.velocity)?))?;
    // a player is pushed: the velocity is added to what it has
    def("SetVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        write(a, |ent| ent.velocity = if ent.player.is_some() { ent.velocity + v } else { v })
    })?;
    def("SetLocalVelocity", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        write(a, |ent| ent.velocity = v)
    })?;

    // its box
    def("OBBMins", |a| vector(a.lua, read(a, |ent| ent.mins)?))?;
    def("OBBMaxs", |a| vector(a.lua, read(a, |ent| ent.maxs)?))?;
    def("OBBCenter", |a| vector(a.lua, read(a, |ent| (ent.mins + ent.maxs) * 0.5)?))?;
    // the point of its box nearest to a point of the world
    def("NearestPoint", |a| {
        let point = a.userdata::<Vector>(2, "Vector")?;
        let (pos, angles) = transform(a)?;
        let (mins, maxs) = read(a, |ent| (ent.mins, ent.maxs))?;
        let local = unrotated(point - pos, angles);
        let clamped = Vector::new(local.x.clamp(mins.x, maxs.x), local.y.clamp(mins.y, maxs.y), local.z.clamp(mins.z, maxs.z));
        vector(a.lua, pos + clamped.rotated(angles))
    })?;
    def("SetCollisionBounds", |a| {
        let (mins, maxs) = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        write(a, |ent| {
            ent.mins = Vector::new(mins.x.min(maxs.x), mins.y.min(maxs.y), mins.z.min(maxs.z));
            ent.maxs = Vector::new(mins.x.max(maxs.x), mins.y.max(maxs.y), mins.z.max(maxs.z));
        })
    })?;
    def("WorldSpaceCenter", |a| {
        let (pos, angles) = transform(a)?;
        let center = read(a, |ent| (ent.mins + ent.maxs) * 0.5)?;
        vector(a.lua, pos + center.rotated(angles))
    })?;
    def("BoundingRadius", |a| Ok(Value::Number(read(a, |ent| ((ent.maxs - ent.mins) * 0.5).length())? as f64)))?;

    // how it looks
    def("GetModel", |a| {
        let model = read(a, |ent| ent.model.clone())?;
        if model.is_empty() { Ok(Value::Nil) } else { string(a.lua, &model) }
    })?;
    def("SetModel", |a| {
        let handle = this(a, 1)?;
        let model = a.string(2)?;
        set_model(a.lua, handle, &model);
        Ok(Value::Nil)
    })?;
    def("GetSkin", |a| integer(read(a, |ent| ent.skin)?))?;
    def("SetSkin", |a| {
        let skin = a.number(2)? as i64;
        write(a, |ent| ent.skin = skin)
    })?;
    def("GetMaterial", |a| {
        let material = read(a, |ent| ent.material.clone())?;
        string(a.lua, &material)
    })?;
    def("SetMaterial", |a| {
        let material = a.opt_string(2)?.unwrap_or_default();
        write(a, |ent| ent.material = material)
    })?;
    def("SetColor4Part", |a| {
        let color = [color_part(a, 2)?, color_part(a, 3)?, color_part(a, 4)?, color_part(a, 5)?];
        write(a, |ent| ent.color = color)
    })?;
    // nothing for the first argument: every slot back to the model's
    def("SetSubMaterial", |a| {
        let slot = a.opt_number(2).ok().flatten().map(|n| n as i64);
        let material = a.opt_string(3).ok().flatten().unwrap_or_default();
        write(a, |ent| match slot {
            None => ent.sub_materials.clear(),
            Some(slot) => {
                ent.sub_materials.retain(|(s, _)| *s != slot);
                if !material.is_empty() {
                    ent.sub_materials.push((slot, material));
                }
            }
        })
    })?;
    def("GetSubMaterial", |a| {
        let slot = a.number(2)? as i64;
        let material = read(a, |ent| ent.sub_materials.iter().find(|(s, _)| *s == slot).map(|(_, m)| m.clone()))?;
        string(a.lua, material.as_deref().unwrap_or(""))
    })?;
    def("GetFlexScale", |a| Ok(Value::Number(f64::from(read(a, |ent| ent.flex_scale)?))))?;
    def("SetFlexScale", |a| {
        let scale = a.number(2)? as f32;
        write(a, |ent| ent.flex_scale = scale)
    })?;
    def("GetFlexWeight", |a| {
        let flex = a.number(2)? as i64;
        let weight = read(a, |ent| ent.flex_weights.iter().find(|(f, _)| *f == flex).map_or(0.0, |&(_, w)| w))?;
        Ok(Value::Number(f64::from(weight)))
    })?;
    def("SetFlexWeight", |a| {
        let (flex, weight) = (a.number(2)? as i64, a.number(3)? as f32);
        write(a, |ent| {
            ent.flex_weights.retain(|(f, _)| *f != flex);
            ent.flex_weights.push((flex, weight));
        })
    })?;
    def("ManipulateBonePosition", |a| {
        let (bone, position) = (a.number(2)? as i64, a.userdata::<Vector>(3, "Vector")?);
        manipulate(a, bone, |m| m.position = position)
    })?;
    def("ManipulateBoneAngles", |a| {
        let (bone, angles) = (a.number(2)? as i64, a.userdata::<Angle>(3, "Angle")?);
        manipulate(a, bone, |m| m.angles = angles)
    })?;
    def("ManipulateBoneScale", |a| {
        let (bone, scale) = (a.number(2)? as i64, a.userdata::<Vector>(3, "Vector")?);
        manipulate(a, bone, |m| m.scale = scale)
    })?;
    def("GetManipulateBonePosition", |a| vector(a.lua, manipulation(a)?.position))?;
    def("GetManipulateBoneAngles", |a| angle(a.lua, manipulation(a)?.angles))?;
    def("GetManipulateBoneScale", |a| vector(a.lua, manipulation(a)?.scale))?;
    def("HasBoneManipulations", |a| Ok(Value::Boolean(read(a, |ent| !ent.bone_manipulations.is_empty())?)))?;
    def("GetRenderFX", |a| integer(read(a, |ent| ent.render_fx)?))?;
    def("SetRenderFX", |a| {
        let fx = a.number(2)? as i64;
        write(a, |ent| ent.render_fx = fx)
    })?;
    def("GetRenderMode", |a| integer(read(a, |ent| ent.render_mode)?))?;
    def("SetRenderMode", |a| {
        let mode = a.number(2)? as i64;
        write(a, |ent| ent.render_mode = mode)
    })?;
    def("GetNoDraw", |a| Ok(Value::Boolean(read(a, |ent| ent.no_draw)?)))?;
    def("SetNoDraw", |a| {
        let no_draw = a.boolean(2);
        write(a, |ent| ent.no_draw = no_draw)
    })?;
    def("DrawShadow", |a| {
        let draw = a.boolean(2);
        write(a, |ent| ent.draw_shadow = draw)
    })?;
    def("GetBodygroup", |a| {
        let id = a.number(2)? as i64;
        integer(read(a, |ent| ent.body_groups.iter().find(|(i, _)| *i == id).map_or(0, |(_, v)| *v))?)
    })?;
    def("SetBodygroup", |a| {
        let (id, value) = (a.number(2)? as i64, a.number(3)? as i64);
        write(a, |ent| match ent.body_groups.iter_mut().find(|(i, _)| *i == id) {
            Some(group) => group.1 = value,
            None => ent.body_groups.push((id, value)),
        })
    })?;
    def("GetNumBodyGroups", |a| integer(studio(a)?.map_or(0, |s| s.body_parts.len() as i64)))?;
    def("GetBodygroupCount", |a| {
        let id = a.number(2)? as usize;
        integer(studio(a)?.and_then(|s| s.body_parts.get(id).map(|(_, models)| models.len() as i64)).unwrap_or(0))
    })?;
    def("GetBodygroupName", |a| {
        let id = a.number(2)? as usize;
        let name = studio(a)?.and_then(|s| s.body_parts.get(id).map(|(name, _)| name.clone())).unwrap_or_default();
        string(a.lua, &name)
    })?;
    def("FindBodygroupByName", |a| {
        let name = a.string(2)?;
        let found = studio(a)?.and_then(|s| s.body_parts.iter().position(|(n, _)| n.eq_ignore_ascii_case(&name)));
        integer(found.map_or(-1, |i| i as i64))
    })?;
    def("GetBodyGroups", |a| {
        let list = a.lua.create_table()?;
        for (id, (name, models)) in studio(a)?.map(|s| s.body_parts).unwrap_or_default().into_iter().enumerate() {
            let group = a.lua.create_table()?;
            group.raw_set("id", id)?;
            group.raw_set("name", name)?;
            group.raw_set("num", models.len())?;
            let submodels = a.lua.create_table()?;
            for (i, model) in models.into_iter().enumerate() {
                submodels.raw_set(i, model)?;
            }
            group.raw_set("submodels", submodels)?;
            list.raw_push(group)?;
        }
        Ok(Value::Table(list))
    })?;
    // one character a body group: 0-9, then a-z for 10 to 35
    def("SetBodyGroups", |a| {
        let values: Vec<i64> = a.string(2)?.chars().map(|c| c.to_digit(36).map_or(0, i64::from)).collect();
        write(a, |ent| {
            for (id, value) in values.into_iter().enumerate() {
                match ent.body_groups.iter_mut().find(|(i, _)| *i == id as i64) {
                    Some(group) => group.1 = value,
                    None => ent.body_groups.push((id as i64, value)),
                }
            }
        })
    })?;
    def("SkinCount", |a| integer(studio(a)?.map_or(0, |s| s.skins as i64)))?;
    // decals and eyes are drawn by clients, which are not there yet
    def("RemoveAllDecals", |a| write(a, |_| ()))?;
    def("SetEyeTarget", |a| write(a, |_| ()))?;
    // what was not spawned from a Workshop addon has no ID
    def("GetWorkshopID", |a| this(a, 1).map(|_| Value::Nil))?;
    def("SetSpawnEffect", |a| {
        let on = a.boolean(2);
        write(a, |ent| ent.spawn_effect = on)
    })?;
    def("GetSpawnEffect", |a| Ok(Value::Boolean(read(a, |ent| ent.spawn_effect)?)))?;
    // bones, attachments and flexes, as the model has them
    def("GetBoneCount", |a| integer(studio(a)?.map_or(0, |s| s.bones.len() as i64)))?;
    def("LookupBone", |a| {
        let name = a.string(2)?;
        let found = studio(a)?.and_then(|s| s.bones.iter().position(|(n, _)| n.eq_ignore_ascii_case(&name)));
        found.map_or(Ok(Value::Nil), |i| integer(i as i64))
    })?;
    def("GetBoneName", |a| {
        let bone = a.number(2)? as i64;
        let name = studio(a)?.and_then(|s| usize::try_from(bone).ok().and_then(|b| s.bones.get(b).map(|(n, _)| n.clone())));
        string(a.lua, name.as_deref().unwrap_or("__INVALIDBONE__"))
    })?;
    def("GetBoneParent", |a| {
        let bone = a.number(2)? as i64;
        let parent = studio(a)?.and_then(|s| usize::try_from(bone).ok().and_then(|b| s.bones.get(b).map(|&(_, p)| p)));
        integer(parent.map_or(-1, i64::from))
    })?;
    // 0 for none, -1 without a model
    def("LookupAttachment", |a| {
        let name = a.string(2)?;
        match studio(a)? {
            Some(s) => integer(s.attachments.iter().position(|n| n.eq_ignore_ascii_case(&name)).map_or(0, |i| i as i64 + 1)),
            None => integer(-1),
        }
    })?;
    def("GetAttachments", |a| {
        let list = a.lua.create_table()?;
        for (i, name) in studio(a)?.map(|s| s.attachments).unwrap_or_default().into_iter().enumerate() {
            let attachment = a.lua.create_table()?;
            attachment.raw_set("id", i + 1)?;
            attachment.raw_set("name", name)?;
            list.raw_push(attachment)?;
        }
        Ok(Value::Table(list))
    })?;
    def("GetFlexNum", |a| integer(studio(a)?.map_or(0, |s| s.flexes.len() as i64)))?;
    def("GetFlexName", |a| {
        let flex = a.number(2)? as i64;
        let name = studio(a)?.and_then(|s| usize::try_from(flex).ok().and_then(|f| s.flexes.get(f).cloned()));
        name.map_or(Ok(Value::Nil), |n| string(a.lua, &n))
    })?;
    def("GetFlexIDByName", |a| {
        let name = a.string(2)?;
        let found = studio(a)?.and_then(|s| s.flexes.iter().position(|n| n.eq_ignore_ascii_case(&name)));
        found.map_or(Ok(Value::Nil), |i| integer(i as i64))
    })?;
    def("HasFlexManipulatior", |a| Ok(Value::Boolean(studio(a)?.is_some_and(|s| !s.flexes.is_empty()))))?;
    def("GetModelScale", |a| Ok(Value::Number(read(a, |ent| ent.model_scale)? as f64)))?;
    def("SetModelScale", |a| {
        let scale = a.number(2)? as f32;
        write(a, |ent| ent.model_scale = scale)
    })?;

    // its keys
    def("GetName", |a| {
        let name = read(a, |ent| ent.name.clone())?;
        string(a.lua, &name)
    })?;
    def("SetName", |a| {
        let name = a.string(2)?;
        write(a, |ent| ent.name = name)
    })?;
    def("GetSpawnFlags", |a| integer(read(a, |ent| ent.spawn_flags)?))?;
    def("HasSpawnFlags", |a| {
        let flags = a.number(2)? as i64;
        Ok(Value::Boolean(read(a, |ent| ent.spawn_flags & flags != 0)?))
    })?;
    // an entity for a key, as a rope's ends (`StartEntity`, `EndEntity`)
    def("SetEntity", |a| {
        let key = a.string(2)?;
        let other = handle_of(a.get(3)).unwrap_or(EntityHandle::NULL);
        write(a, |ent| {
            ent.links.retain(|(k, _)| !k.eq_ignore_ascii_case(&key));
            ent.links.push((key, other));
        })
    })?;
    def("SetKeyValue", |a| {
        let handle = this(a, 1)?;
        let key = a.string(2)?;
        let value = match a.get(3) {
            Some(v) => a.lua.coerce_string(v.clone())?.map(|s| s.to_string_lossy()).unwrap_or_default(),
            None => String::new(),
        };
        crate::ents::key_value(a.lua, handle, &key, &value);
        Ok(Value::Nil)
    })?;
    // the engine's own fields of an entity, by their names in its data map; those Lua reads
    def("GetInternalVariable", |a| {
        let handle = this(a, 1)?;
        let name = a.string(2)?;
        let (_, angles) = host(a.lua).entities.transform(handle).expect("checked above");
        let ent = host(a.lua).entities.get(handle).expect("checked above").clone();
        let number = |key: &str| ent.keyvalue(key).and_then(|v| v.trim().parse::<f32>().ok());
        Ok(match name.as_str() {
            // towards the sun: the light's direction as Source sets it up from a yaw (`angle`,
            // else the angles') and a `pitch` (negative is down, else the angles'), turned round
            "m_vDirection" => {
                let yaw = number("angle").filter(|y| *y != -1.0).unwrap_or(angles.y).to_radians();
                let pitch = number("pitch").unwrap_or(-angles.p).to_radians();
                let light = Vector::new(yaw.cos() * pitch.cos(), yaw.sin() * pitch.cos(), pitch.sin());
                vector(a.lua, Vector::new(-light.x, -light.y, -light.z))?
            }
            "m_iName" => Value::String(a.lua.create_string(&ent.name)?),
            "m_ModelName" => Value::String(a.lua.create_string(&ent.model)?),
            "m_nSkin" => Value::Integer(ent.skin),
            "m_spawnflags" => Value::Integer(ent.spawn_flags),
            _ => Value::Nil,
        })
    })?;
    def("GetKeyValues", |a| {
        let handle = this(a, 1)?;
        let (pos, angles) = host(a.lua).entities.transform(handle).expect("checked above");
        let ent = host(a.lua).entities.get(handle).expect("checked above").clone();
        let t = a.lua.create_table()?;
        for (key, value) in &ent.keyvalues {
            t.raw_set(key.as_str(), value.as_str())?;
        }
        t.raw_set("classname", ent.class.as_str())?;
        t.raw_set("targetname", ent.name.as_str())?;
        t.raw_set("origin", format!("{} {} {}", pos.x, pos.y, pos.z))?;
        t.raw_set("angles", format!("{} {} {}", angles.p, angles.y, angles.r))?;
        t.raw_set("model", ent.model.as_str())?;
        t.raw_set("skin", ent.skin)?;
        t.raw_set("spawnflags", ent.spawn_flags)?;
        t.raw_set("health", ent.health)?;
        t.raw_set("max_health", ent.max_health)?;
        Ok(Value::Table(t))
    })?;

    // health
    def("Health", |a| integer(read(a, |ent| ent.health)?))?;
    def("SetHealth", |a| {
        let health = a.number(2)? as i64;
        write(a, |ent| ent.health = health)
    })?;
    def("GetMaxHealth", |a| integer(read(a, |ent| ent.max_health)?))?;
    def("SetMaxHealth", |a| {
        let health = a.number(2)? as i64;
        write(a, |ent| ent.max_health = health)
    })?;

    // owner and parent
    def("GetOwner", |a| {
        let owner = read(a, |ent| ent.owner)?;
        push(a.lua, owner)
    })?;
    def("SetOwner", |a| {
        let owner = opt_entity(a, 2)?.unwrap_or(EntityHandle::NULL);
        write(a, |ent| ent.owner = owner)
    })?;
    def("GetParent", |a| {
        let parent = read(a, |ent| ent.parent)?;
        push(a.lua, parent)
    })?;
    def("SetParent", |a| {
        let handle = this(a, 1)?;
        let parent = opt_entity(a, 2)?.unwrap_or(EntityHandle::NULL);
        set_parent(a.lua, handle, parent);
        Ok(Value::Nil)
    })?;
    def("GetChildren", |a| {
        let handle = this(a, 1)?;
        let children: Vec<EntityHandle> = {
            let entities = &host(a.lua).entities;
            entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|ent| ent.parent == handle)).collect()
        };
        let list = a.lua.create_table()?;
        for child in children {
            list.raw_push(push(a.lua, child)?)?;
        }
        Ok(Value::Table(list))
    })?;

    // how it moves and collides
    def("GetMoveType", |a| integer(read(a, |ent| ent.move_type)?))?;
    def("SetMoveType", |a| {
        let move_type = a.number(2)? as i64;
        write(a, |ent| ent.move_type = move_type)
    })?;
    def("GetSolid", |a| integer(read(a, |ent| ent.solid)?))?;
    def("SetSolid", |a| {
        let solid = a.number(2)? as i64;
        write(a, |ent| ent.solid = solid)
    })?;
    def("GetSolidFlags", |a| integer(read(a, |ent| ent.solid_flags)?))?;
    def("SetSolidFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.solid_flags = flags)
    })?;
    def("AddSolidFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.solid_flags |= flags)
    })?;
    def("RemoveSolidFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.solid_flags &= !flags)
    })?;
    def("SetNotSolid", |a| {
        let not_solid = a.boolean(2);
        // FSOLID_NOT_SOLID
        write(a, |ent| if not_solid { ent.solid_flags |= 4 } else { ent.solid_flags &= !4 })
    })?;
    def("IsSolid", |a| Ok(Value::Boolean(read(a, |ent| ent.solid != 0 && ent.solid_flags & 4 == 0)?)))?;
    def("GetCollisionGroup", |a| integer(read(a, |ent| ent.collision_group)?))?;
    def("SetCollisionGroup", |a| {
        let group = a.number(2)? as i64;
        write(a, |ent| ent.collision_group = group)
    })?;
    def("SetUseType", |a| {
        let use_type = a.number(2)? as i64;
        write(a, |ent| ent.use_type = use_type)
    })?;
    def("SetTrigger", |a| {
        let trigger = a.boolean(2);
        write(a, |ent| ent.trigger = trigger)
    })?;
    def("EnableCustomCollisions", |a| {
        let enable = a.boolean(2);
        write(a, |ent| ent.custom_collisions = enable)
    })?;
    def("GetPersistent", |a| Ok(Value::Boolean(read(a, |ent| ent.persistent)?)))?;
    def("SetPersistent", |a| {
        let persistent = a.boolean(2);
        write(a, |ent| ent.persistent = persistent)
    })?;
    def("SetTransmitWithParent", |a| {
        let with_parent = a.boolean(2);
        write(a, |ent| ent.transmit_with_parent = with_parent)
    })?;
    def("GetTrigger", |a| Ok(Value::Boolean(read(a, |ent| ent.trigger)?)))?;
    def("IsOnGround", |a| Ok(Value::Boolean(read(a, |ent| ent.flags & 1 != 0)?)))?;
    def("OnGround", |a| Ok(Value::Boolean(read(a, |ent| ent.flags & 1 != 0)?)))?;
    def("WaterLevel", |a| {
        let handle = this(a, 1)?;
        integer(crate::player::water_level(a.lua, handle).unwrap_or(0).into())
    })?;
    def("IsInWorld", |a| Ok(Value::Boolean(crate::trace::in_world(a.lua, transform(a)?.0))))?;
    // physics: the entity is SOLID_VPHYSICS and MOVETYPE_VPHYSICS (6) with a body or without
    def("PhysicsInit", |a| {
        let solid = a.number(2)? as i64;
        write(a, |ent| {
            ent.solid = solid;
            ent.move_type = 6;
        })?;
        let handle = this(a, 1)?;
        let made = solid == SOLID_VPHYSICS && crate::physics::init_model(a.lua, handle);
        if !made {
            crate::physics::destroy(a.lua, handle);
        }
        Ok(Value::Boolean(made))
    })?;
    def("PhysicsInitStatic", |a| {
        let solid = a.number(2)? as i64;
        write(a, |ent| ent.solid = solid)?;
        let handle = this(a, 1)?;
        let made = crate::physics::init_model(a.lua, handle);
        crate::physics::set_motion(a.lua, handle, false);
        Ok(Value::Boolean(made))
    })?;
    // a shadow follows where Lua puts it: frozen, for now
    def("PhysicsInitShadow", |a| {
        let handle = this(a, 1)?;
        write(a, |ent| ent.move_type = 6)?;
        let made = crate::physics::init_model(a.lua, handle);
        crate::physics::set_motion(a.lua, handle, false);
        Ok(Value::Boolean(made))
    })?;
    def("PhysicsInitBox", |a| {
        let (mins, maxs) = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        write(a, |ent| {
            (ent.mins, ent.maxs) = (mins, maxs);
            ent.solid = SOLID_VPHYSICS;
            ent.move_type = 6;
        })?;
        let shape = gmod_physics::Shape::cuboid([mins.x, mins.y, mins.z], [maxs.x, maxs.y, maxs.z]);
        Ok(Value::Boolean(crate::physics::init_shape(a.lua, this(a, 1)?, shape)))
    })?;
    def("PhysicsInitSphere", |a| {
        let radius = a.number(2)? as f32;
        write(a, |ent| {
            (ent.mins, ent.maxs) = (Vector::new(-radius, -radius, -radius), Vector::new(radius, radius, radius));
            ent.solid = SOLID_VPHYSICS;
            ent.move_type = 6;
        })?;
        Ok(Value::Boolean(crate::physics::init_shape(a.lua, this(a, 1)?, gmod_physics::Shape::Sphere(radius))))
    })?;
    def("PhysicsInitConvex", |a| {
        let points = convex(&a.table(2)?)?;
        write(a, |ent| {
            ent.solid = SOLID_VPHYSICS;
            ent.move_type = 6;
        })?;
        Ok(Value::Boolean(crate::physics::init_shape(a.lua, this(a, 1)?, gmod_physics::Shape::Hulls(vec![points]))))
    })?;
    def("PhysicsInitMultiConvex", |a| {
        let hulls = a.table(2)?.sequence_values::<Table>().map(|t| convex(&t?)).collect::<mlua::Result<Vec<_>>>()?;
        write(a, |ent| {
            ent.solid = SOLID_VPHYSICS;
            ent.move_type = 6;
        })?;
        Ok(Value::Boolean(crate::physics::init_shape(a.lua, this(a, 1)?, gmod_physics::Shape::Hulls(hulls))))
    })?;
    def("PhysicsDestroy", |a| {
        crate::physics::destroy(a.lua, this(a, 1)?);
        Ok(Value::Nil)
    })?;
    def("PhysWake", |a| {
        crate::physics::wake(a.lua, this(a, 1)?);
        Ok(Value::Nil)
    })?;
    // motion controllers are not simulated yet
    def("StartMotionController", |a| write(a, |_| ()))?;
    def("StopMotionController", |a| write(a, |_| ()))?;
    def("GetPhysicsObject", |a| crate::physics::push(a.lua, this(a, 1)?))?;
    def("GetPhysicsObjectNum", |a| {
        let handle = this(a, 1)?;
        if a.number(2)? as i64 == 0 { crate::physics::push(a.lua, handle) } else { crate::physics::push(a.lua, EntityHandle::NULL) }
    })?;
    def("GetPhysicsObjectCount", |a| integer(i64::from(crate::physics::body(a.lua, this(a, 1)?).is_some())))?;

    // flags
    def("GetFlags", |a| integer(read(a, |ent| ent.flags)?))?;
    def("AddFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.flags |= flags)
    })?;
    def("RemoveFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.flags &= !flags)
    })?;
    def("IsFlagSet", |a| {
        let flags = a.number(2)? as i64;
        Ok(Value::Boolean(read(a, |ent| ent.flags & flags != 0)?))
    })?;
    def("AddEFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.engine_flags |= flags)
    })?;
    def("RemoveEFlags", |a| {
        let flags = a.number(2)? as i64;
        write(a, |ent| ent.engine_flags &= !flags)
    })?;
    def("IsEFlagSet", |a| {
        let flags = a.number(2)? as i64;
        Ok(Value::Boolean(read(a, |ent| ent.engine_flags & flags != 0)?))
    })?;
    def("GetEffects", |a| integer(read(a, |ent| ent.effects)?))?;
    def("AddEffects", |a| {
        let effects = a.number(2)? as i64;
        write(a, |ent| ent.effects |= effects)
    })?;
    def("RemoveEffects", |a| {
        let effects = a.number(2)? as i64;
        write(a, |ent| ent.effects &= !effects)
    })?;
    def("IsEffectActive", |a| {
        let effects = a.number(2)? as i64;
        Ok(Value::Boolean(read(a, |ent| ent.effects & effects != 0)?))
    })?;

    // its life
    def("Spawn", |a| {
        let handle = this(a, 1)?;
        crate::ents::spawn(a.lua, handle);
        Ok(Value::Nil)
    })?;
    def("Activate", |a| {
        crate::ents::activate(a.lua, this(a, 1)?);
        Ok(Value::Nil)
    })?;
    // players leave rather than go
    def("Remove", |a| {
        let handle = this(a, 1)?;
        if handle.kind != Kind::Player {
            crate::ents::remove(a.lua, handle);
        }
        Ok(Value::Nil)
    })?;
    def("DeleteOnRemove", |a| {
        let other = this(a, 2)?;
        let list = deleted_with(a)?;
        if !list.sequence_values::<Value>().flatten().any(|v| handle_of(Some(&v)) == Some(other)) {
            list.raw_push(push(a.lua, other)?)?;
        }
        Ok(Value::Nil)
    })?;
    def("DontDeleteOnRemove", |a| {
        let other = this(a, 2)?;
        let list = deleted_with(a)?;
        let kept: Vec<Value> = list.sequence_values::<Value>().flatten().filter(|v| handle_of(Some(v)) != Some(other)).collect();
        list.clear()?;
        for value in kept {
            list.raw_push(value)?;
        }
        Ok(Value::Nil)
    })?;
    // callbacks by hook name, each a list of functions under IDs from 1
    def("AddCallback", |a| {
        let callbacks = callbacks(a)?;
        let (name, function) = (a.string(2)?, a.function(3)?);
        let list = match callbacks.raw_get::<Option<Table>>(name.as_str())? {
            Some(list) => list,
            None => {
                let list = a.lua.create_table()?;
                callbacks.raw_set(name, &list)?;
                list
            }
        };
        let id = list.raw_len() + 1;
        list.raw_set(id, function)?;
        integer(id as i64)
    })?;
    def("GetCallbacks", |a| {
        let callbacks = callbacks(a)?;
        let list = callbacks.raw_get::<Option<Table>>(a.string(2)?)?;
        Ok(Value::Table(match list {
            Some(list) => list,
            None => a.lua.create_table()?,
        }))
    })?;
    def("RemoveCallback", |a| {
        let callbacks = callbacks(a)?;
        if let Some(list) = callbacks.raw_get::<Option<Table>>(a.string(2)?)? {
            list.raw_set(a.number(3)? as i64, Value::Nil)?;
        }
        Ok(Value::Nil)
    })?;
    def("NextThink", |a| {
        let time = a.number(2)?;
        write(a, |ent| ent.think_request = Some(time))
    })?;

    crate::inputs::install_methods(def)?;
    crate::constraint::install_methods(def)?;
    crate::sound::install_methods(def)?;
    crate::ents::install_variables(def)
}

/// The entities removed with this one (`DeleteOnRemove`).
fn deleted_with(a: &Args) -> mlua::Result<Table> {
    match a.get(1) {
        Some(Value::UserData(ud)) => store_of(a.lua, ud, "delete-on-remove"),
        _ => Err(a.bad(1, "Entity")),
    }
}

/// What the model of the entity in argument 1 is made of.
/// Changes how a bone of the entity in argument 1 is moved; one moved nowhere is forgotten.
fn manipulate(a: &Args, bone: i64, f: impl FnOnce(&mut BoneManipulation)) -> mlua::Result<Value> {
    write(a, |ent| {
        let mut m = ent.bone_manipulations.iter().find(|m| m.bone == bone).copied().unwrap_or(BoneManipulation::new(bone));
        f(&mut m);
        ent.bone_manipulations.retain(|other| other.bone != bone);
        if !m.is_none() {
            ent.bone_manipulations.push(m);
        }
    })
}

/// How the bone in argument 2 of the entity in argument 1 is moved.
fn manipulation(a: &Args) -> mlua::Result<BoneManipulation> {
    let bone = a.number(2)? as i64;
    read(a, |ent| ent.bone_manipulations.iter().find(|m| m.bone == bone).copied().unwrap_or(BoneManipulation::new(bone)))
}

fn studio(a: &Args) -> mlua::Result<Option<crate::ents::Studio>> {
    let model = read(a, |ent| ent.model.clone())?;
    Ok(crate::ents::studio(a.lua, &model))
}

/// The entity's table of callbacks (`AddCallback`).
fn callbacks(a: &Args) -> mlua::Result<Table> {
    this(a, 1)?;
    match a.get(1) {
        Some(Value::UserData(ud)) => store_of(a.lua, ud, "callbacks"),
        _ => Err(a.bad(1, "Entity")),
    }
}

/// Gives an entity a parent (or none), keeping where it is in the world. A parent that is
/// the entity itself or one of its children is refused, as the engine does.
pub(crate) fn set_parent(lua: &Lua, handle: EntityHandle, parent: EntityHandle) {
    let entities = &host(lua).entities;
    let mut ancestor = parent;
    for _ in 0..32 {
        if ancestor == handle {
            return;
        }
        match entities.get(ancestor) {
            Some(ent) => ancestor = ent.parent,
            None => break,
        }
    }
    let Some((pos, angles)) = entities.transform(handle) else { return };
    entities.with_mut(handle, |ent| ent.parent = parent);
    set_world_pos(lua, handle, pos);
    set_world_angles(lua, handle, angles);
}
