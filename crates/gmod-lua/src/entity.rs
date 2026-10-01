//! Entities as Lua sees them, and the named metatables of the engine's other types.
//!
//! There are no entities yet (they come with stage 4); what is here is what the game's Lua
//! needs before any exists: the metatables it extends (`Entity`, `Player`, `Weapon` ...), and
//! `NULL`, the entity that is not there.

use mlua::{AnyUserData, Lua, MultiValue, UserData, UserDataFields, UserDataMethods, Value};

use crate::Realm;
use crate::args::{self, Args};
use crate::meta::{self, type_id};

/// A handle to an entity: its index, or none for `NULL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityHandle {
    pub index: Option<u32>,
}

impl EntityHandle {
    pub const NULL: EntityHandle = EntityHandle { index: None };
}

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

impl UserData for EntityHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "Entity");
        fields.add_meta_field("MetaID", type_id::ENTITY);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (_, key): (AnyUserData, Value)| meta::lookup(lua, "Entity", key));
        methods.add_meta_function("__newindex", |lua, (ud, _, _): (AnyUserData, Value, Value)| -> mlua::Result<()> {
            match ud.borrow::<EntityHandle>()?.index {
                None => Err(args::error(lua, "Tried to use a NULL entity!")),
                Some(_) => Ok(()),
            }
        });
        methods.add_meta_function("__eq", |_, (a, b): (AnyUserData, AnyUserData)| {
            Ok(matches!((a.borrow::<EntityHandle>(), b.borrow::<EntityHandle>()), (Ok(a), Ok(b)) if *a == *b))
        });
        methods.add_meta_function("__tostring", |_, ud: AnyUserData| {
            Ok(match ud.borrow::<EntityHandle>()?.index {
                None => "[NULL Entity]".to_string(),
                Some(index) => format!("Entity [{index}]"),
            })
        });
    }
}

pub(crate) fn install(lua: &Lua, realm: Realm) -> mlua::Result<()> {
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

    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        entity.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("IsValid", |a| Ok(Value::Boolean(handle(a)?.index.is_some())))?;
    def("EntIndex", |a| Ok(Value::Integer(handle(a)?.index.map_or(0, i64::from))))?;
    def("IsWorld", |a| Ok(Value::Boolean(handle(a)?.index == Some(0))))?;

    lua.globals().raw_set("NULL", EntityHandle::NULL)?;
    Ok(())
}

fn handle(args: &Args) -> mlua::Result<EntityHandle> {
    args.userdata::<EntityHandle>(1, "Entity")
}
