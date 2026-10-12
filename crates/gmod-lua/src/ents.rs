//! The life of entities and the `ents` library: creating them (`ents.Create`, the entities of
//! a map), spawning, thinking and removing them, finding them, and the variables the engine
//! keeps with them (`SetNWInt`, `SetDTFloat` ...).
//!
//! A scripted entity (SENT) or weapon (SWEP) gets its table from `scripted_ents.Get` or
//! `weapons.Get`, as in GMod, then `InstallDataTable` and `SetupDataTables`; `Spawn` runs its
//! `Initialize`, and its `Think` runs every [`DEFAULT_THINK`] seconds unless it asks for
//! another time with `NextThink` and returns true. Removing an entity runs `OnRemove` and the
//! `EntityRemoved` hook at once; it is gone at the end of the frame.
//!
//! The engine's own entity classes (`prop_physics`, `func_door`, `logic_relay` ...) exist as
//! entities with their key-values but do nothing yet: their behaviour, like physics, comes
//! later in stage 4.

use std::cell::RefCell;
use std::collections::HashMap;

use mlua::{AnyUserData, Function, Lua, MultiValue, ObjectLike, Table, Value};

use crate::args::Args;
use crate::entity::{self, Ent, EntityHandle, Kind};
use crate::vector::{Angle, Vector};
use crate::{Realm, ScriptError, files, host};

/// How often a SENT's `Think` runs on the server when it does not ask for another time: "about
/// 5-6 times per second", as GMod's wiki puts it.
pub const DEFAULT_THINK: f64 = 0.2;

/// Map creation IDs of the map's entities: this, plus the entity's place in the entity lump.
pub const FIRST_MAP_CREATION_ID: i32 = 1234;

/// A map as the server loads it.
#[derive(Debug, Clone, Default)]
pub struct Map {
    /// `gm_construct`, what `game.GetMap` returns.
    pub name: String,
    /// The entity lump: each entity's key-values in file order.
    pub entities: Vec<Vec<(String, String)>>,
    /// Mins and maxs of the brush models; `*0` is the world.
    pub brush_models: Vec<([f32; 3], [f32; 3])>,
    /// What traces and players run into ([`gmod_bsp::Collision`]); without it the world is
    /// empty space.
    pub collision: Option<std::sync::Arc<gmod_bsp::Collision>>,
    /// The material of each surface (`texinfo`), which traces name as `HitTexture`.
    pub textures: Vec<String>,
}

impl Map {
    /// The map `bsp` named `name` (`gm_construct`), solid as `collision` says.
    pub fn from_bsp(name: &str, bsp: &gmod_bsp::Bsp, collision: std::sync::Arc<gmod_bsp::Collision>) -> Map {
        let texture = |texdata: i32| usize::try_from(texdata).ok().and_then(|i| bsp.materials.get(i)).cloned().unwrap_or_default();
        Map {
            name: name.to_string(),
            entities: bsp.entities.iter().map(|e| e.pairs.clone()).collect(),
            brush_models: bsp.models.iter().map(|m| (m.mins, m.maxs)).collect(),
            collision: Some(collision),
            textures: bsp.texinfo.iter().map(|info| texture(info.texdata)).collect(),
        }
    }
}

/// The models entities have, as far as the engine reads them: the bounds of the brush models
/// of the map, and what `SetModel` takes from a studio model.
#[derive(Default)]
pub(crate) struct Models {
    brush: RefCell<Vec<(Vector, Vector)>>,
    studio: RefCell<HashMap<String, Option<Studio>>>,
}

/// What the engine takes from a studio model (`.mdl`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Studio {
    pub hull: (Vector, Vector),
    /// The body groups: the name of each, and the names of its models.
    pub body_parts: Vec<(String, Vec<String>)>,
    /// How many skins it has.
    pub skins: usize,
    /// The bones: the name of each, and its parent's number (-1 for none).
    pub bones: Vec<(String, i32)>,
    /// The names of the attachments, numbered from 1.
    pub attachments: Vec<String>,
    /// The names of the flexes.
    pub flexes: Vec<String>,
    /// Its `$keyvalues` text (`prop_data` ...).
    pub key_values: String,
}

/// A studio model, read once.
pub(crate) fn studio(lua: &Lua, model: &str) -> Option<Studio> {
    if model.is_empty() || model.starts_with('*') {
        return None;
    }
    let host = host(lua);
    let path = model.replace('\\', "/").to_ascii_lowercase();
    if let Some(studio) = host.models.studio.borrow().get(&path) {
        return studio.clone();
    }
    let studio = host.vfs.read(&path, "GAME").ok().and_then(|bytes| gmod_model::Mdl::parse(&bytes).ok()).map(|mdl| {
        let (lo, hi) = mdl.hull;
        Studio {
            hull: (Vector::new(lo[0], lo[1], lo[2]), Vector::new(hi[0], hi[1], hi[2])),
            body_parts: mdl
                .body_parts
                .iter()
                .map(|part| (part.name.clone(), part.models.iter().map(|m| m.name.clone()).collect()))
                .collect(),
            skins: mdl.skins.len().max(1),
            bones: mdl.bones.iter().map(|b| (b.name.clone(), b.parent)).collect(),
            attachments: mdl.attachments.iter().map(|a| a.name.clone()).collect(),
            flexes: mdl.flexes.clone(),
            key_values: mdl.key_values.clone(),
        }
    });
    host.models.studio.borrow_mut().insert(path, studio.clone());
    studio
}

/// The collision box a model gives an entity: the hull of a studio model (`.mdl`), the
/// bounds of a brush model of the map (`*3`).
pub(crate) fn model_bounds(lua: &Lua, model: &str) -> Option<(Vector, Vector)> {
    if let Some(number) = model.strip_prefix('*') {
        let number: usize = number.parse().ok()?;
        return host(lua).models.brush.borrow().get(number).copied();
    }
    studio(lua, model).map(|studio| studio.hull)
}

/// The classes of entity the engine itself has (Source's and GMod's C++ ones) that
/// `ents.Create` makes. The entities of a map are made whatever their class.
#[rustfmt::skip]
const ENGINE_CLASSES: &[&str] = &[
    "ai_ally_manager", "ai_battle_line", "ai_changehintgroup", "ai_changetarget", "ai_goal_actbusy", "ai_goal_assault",
    "ai_goal_follow", "ai_goal_lead", "ai_goal_standoff", "ai_relationship", "ai_script_conditions", "ai_speechfilter",
    "aiscripted_schedule", "ambient_generic", "assault_assaultpoint", "assault_rallypoint", "beam", "color_correction",
    "color_correction_volume", "combine_mine", "crossbow_bolt", "cycler", "env_ar2explosion", "env_beam", "env_blood",
    "env_citadel_energy_core", "env_credits", "env_dustpuff", "env_effectscript", "env_embers", "env_entity_dissolver",
    "env_entity_igniter", "env_entity_maker", "env_explosion", "env_fade", "env_fire", "env_firesensor",
    "env_firesource", "env_flare", "env_fog_controller", "env_funnel", "env_global", "env_gunfire",
    "env_headcrabcanister", "env_hudhint", "env_laser", "env_lightglow", "env_message", "env_microphone",
    "env_movieexplosion", "env_particlescript", "env_physexplosion", "env_physimpact", "env_player_surface_trigger",
    "env_projectedtexture", "env_rotorwash_emitter", "env_screeneffect", "env_screenoverlay", "env_shake",
    "env_shooter", "env_smokestack", "env_smoketrail", "env_soundscape", "env_soundscape_proxy",
    "env_soundscape_triggerable", "env_spark", "env_speaker", "env_splash", "env_sprite", "env_sprite_oriented",
    "env_spritetrail", "env_steam", "env_sun", "env_terrainmorph", "env_texturetoggle", "env_tonemap_controller",
    "env_viewpunch", "env_wind", "env_zoom", "entityflame", "filter_activator_class", "filter_activator_mass_greater",
    "filter_activator_name", "filter_activator_team", "filter_damage_type", "filter_multi", "func_areaportal",
    "func_areaportalwindow", "func_breakable", "func_breakable_surf", "func_brush", "func_button", "func_clip_vphysics",
    "func_combine_ball_spawner", "func_conveyor", "func_door", "func_door_rotating", "func_dustcloud", "func_dustmotes",
    "func_fish_pool", "func_guntarget", "func_illusionary", "func_lod", "func_lookdoor", "func_monitor",
    "func_movelinear", "func_occluder", "func_physbox", "func_physbox_multiplayer", "func_platrot",
    "func_precipitation", "func_recharge", "func_reflective_glass", "func_rot_button", "func_rotating",
    "func_smokevolume", "func_tank", "func_tanktrain", "func_tracktrain", "func_train", "func_useableladder",
    "func_vehicleclip", "func_wall", "func_wall_toggle", "func_water_analog", "game_end", "game_player_equip",
    "game_ragdoll_manager", "game_score", "game_text", "game_ui", "game_weapon_manager", "game_zone_player",
    "generic_actor", "gib", "grenade_ar2", "grenade_helicopter", "hunter_flechette", "info_camera_link",
    "info_landmark", "info_lighting", "info_node", "info_node_air", "info_node_hint", "info_particle_system",
    "info_player_allies", "info_player_axis", "info_player_combine", "info_player_counterterrorist",
    "info_player_deathmatch", "info_player_rebel", "info_player_start", "info_player_teamspawn",
    "info_player_terrorist", "info_projecteddecal", "info_target", "info_teleport_destination", "infodecal",
    "item_ammo_357", "item_ammo_357_large", "item_ammo_ar2", "item_ammo_ar2_altfire", "item_ammo_ar2_large",
    "item_ammo_crate", "item_ammo_crossbow", "item_ammo_pistol", "item_ammo_pistol_large", "item_ammo_smg1",
    "item_ammo_smg1_grenade", "item_ammo_smg1_large", "item_ar2_grenade", "item_battery", "item_box_buckshot",
    "item_box_lrounds", "item_box_mrounds", "item_box_srounds", "item_dynamic_resupply", "item_healthcharger",
    "item_healthkit", "item_healthvial", "item_item_crate", "item_large_box_lrounds", "item_large_box_mrounds",
    "item_large_box_srounds", "item_rpg_round", "item_suit", "item_suitcharger", "keyframe_rope", "light", "light_dynamic", "light_environment", "light_spot", "logic_auto",
    "logic_branch", "logic_branch_listener", "logic_case", "logic_collision_pair", "logic_compare", "logic_lineto",
    "logic_measure_movement", "logic_multicompare", "logic_navigation", "logic_playerproxy", "logic_relay",
    "logic_timer", "math_colorblend", "math_counter", "math_remap", "momentary_rot_button", "move_rope", "npc_alyx",
    "npc_antlion", "npc_antlion_grub", "npc_antlion_template_maker", "npc_antlion_worker", "npc_antlionguard",
    "npc_barnacle", "npc_barney", "npc_breen", "npc_bullseye", "npc_citizen", "npc_clawscanner", "npc_combine_camera",
    "npc_combine_s", "npc_combinedropship", "npc_combinegunship", "npc_crow", "npc_cscanner", "npc_dog", "npc_eli",
    "npc_enemyfinder", "npc_fastzombie", "npc_fastzombie_torso", "npc_fisherman", "npc_gman", "npc_grenade_frag",
    "npc_headcrab", "npc_headcrab_black", "npc_headcrab_fast", "npc_headcrab_poison", "npc_helicopter", "npc_hunter",
    "npc_kleiner", "npc_launcher", "npc_magnusson", "npc_maker", "npc_manhack", "npc_metropolice", "npc_monk",
    "npc_mossman", "npc_pigeon", "npc_poisonzombie", "npc_rollermine", "npc_seagull", "npc_sniper", "npc_stalker",
    "npc_strider", "npc_template_maker", "npc_turret_ceiling", "npc_turret_floor", "npc_turret_ground",
    "npc_vortigaunt", "npc_zombie", "npc_zombie_torso", "npc_zombine", "path_corner", "path_corner_crash", "path_track",
    "phys_ballsocket", "phys_constraint", "phys_constraintsystem", "phys_convert", "phys_hinge", "phys_keepupright",
    "phys_lengthconstraint", "phys_magnet", "phys_motor", "phys_pulleyconstraint", "phys_ragdollconstraint",
    "phys_ragdollmagnet", "phys_slideconstraint", "phys_spring", "phys_thruster", "phys_torque", "physics_cannister",
    "player_loadsaved", "player_speedmod", "player_weaponstrip", "point_camera", "point_clientcommand",
    "point_combine_ball_launcher", "point_commentary_node", "point_hurt", "point_message", "point_playermoveconstraint",
    "point_posecontroller", "point_proximity_sensor", "point_push", "point_servercommand", "point_spotlight",
    "point_teleport", "point_template", "point_tesla", "point_velocitysensor", "point_viewcontrol", "prop_combine_ball",
    "prop_door_rotating", "prop_dynamic", "prop_dynamic_ornament", "prop_dynamic_override", "prop_physics",
    "prop_physics_multiplayer", "prop_physics_override", "prop_physics_respawnable", "prop_ragdoll", "prop_sphere",
    "prop_thumper", "prop_vehicle_airboat", "prop_vehicle_apc", "prop_vehicle_crane", "prop_vehicle_jeep",
    "prop_vehicle_prisoner_pod", "raggib", "rpg_missile", "scripted_scene", "scripted_sequence", "scripted_target",
    "shadow_control", "sky_camera", "spotlight_end", "trigger_autosave", "trigger_changelevel", "trigger_gravity",
    "trigger_hurt", "trigger_impact", "trigger_look", "trigger_multiple", "trigger_once", "trigger_physics_trap",
    "trigger_playermovement", "trigger_proximity", "trigger_push", "trigger_remove", "trigger_rpgfire",
    "trigger_serverragdoll", "trigger_soundscape", "trigger_teleport", "trigger_teleport_relative",
    "trigger_togglesave", "trigger_transition", "trigger_vphysics_motion", "trigger_waterydeath",
    "trigger_weapon_dissolve", "trigger_weapon_strip", "trigger_wind", "vgui_screen", "water_lod_control", "weapon_357",
    "weapon_alyxgun", "weapon_annabelle", "weapon_ar2", "weapon_bugbait", "weapon_crossbow", "weapon_crowbar",
    "weapon_frag", "weapon_physcannon", "weapon_physgun", "weapon_pistol", "weapon_rpg", "weapon_shotgun",
    "weapon_slam", "weapon_smg1", "weapon_stunstick",
];

/// The kind of an engine class: its prefix says it, but for the makers, thrown things and the
/// helicopter's markers, which are named `npc_` and are no NPCs.
fn engine_kind(class: &str) -> Kind {
    let class = class.to_ascii_lowercase();
    let not_npc = matches!(
        class.as_str(),
        "npc_maker"
            | "npc_template_maker"
            | "npc_antlion_template_maker"
            | "npc_grenade_frag"
            | "npc_grenade_bugbait"
            | "npc_satchel"
            | "npc_tripmine"
            | "npc_heli_avoidsphere"
            | "npc_heli_avoidbox"
            | "npc_heli_nobomb"
    );
    if class.starts_with("npc_") && !not_npc {
        Kind::Npc
    } else if class.starts_with("weapon_") {
        Kind::Weapon
    } else if class.starts_with("prop_vehicle") {
        Kind::Vehicle
    } else {
        Kind::Entity
    }
}

/// Where a scripted class's table comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Sent,
    Swep,
}

/// Whether `class` is a SENT or SWEP the game's Lua registered, and its kind.
fn script_of(lua: &Lua, class: &str) -> mlua::Result<Option<(Kind, Script)>> {
    let globals = lua.globals();
    if let Some(sents) = globals.get::<Option<Table>>("scripted_ents")? {
        let stored = match sents.get::<Option<Function>>("GetStored")? {
            Some(f) => f.call::<Value>(class)?,
            None => Value::Nil,
        };
        if !stored.is_nil() {
            let kind = match sents.get::<Option<Function>>("GetType")? {
                Some(f) => match f.call::<Option<String>>(class)?.as_deref() {
                    Some("nextbot") => Kind::NextBot,
                    Some("ai") => Kind::Npc,
                    _ => Kind::Entity,
                },
                None => Kind::Entity,
            };
            return Ok(Some((kind, Script::Sent)));
        }
    }
    if let Some(weapons) = globals.get::<Option<Table>>("weapons")?
        && let Some(f) = weapons.get::<Option<Function>>("GetStored")?
        && !f.call::<Value>(class)?.is_nil()
    {
        return Ok(Some((Kind::Weapon, Script::Swep)));
    }
    Ok(None)
}

/// The userdata of a valid entity.
fn userdata(lua: &Lua, handle: EntityHandle) -> Option<AnyUserData> {
    match entity::push(lua, handle) {
        Ok(Value::UserData(ud)) if host(lua).entities.is_valid(handle) => Some(ud),
        _ => None,
    }
}

/// Calls `ent:name(...)` if the entity has such a function: what it returned, or nothing
/// when it has none or it failed (the error is reported).
pub(crate) fn call_method(lua: &Lua, ud: &AnyUserData, name: &str, args: MultiValue) -> Option<MultiValue> {
    let function = match ud.get::<Value>(name) {
        Ok(Value::Function(f)) => f,
        Ok(_) => return None,
        Err(e) => {
            host(lua).report(ScriptError::from_lua(&e));
            return None;
        }
    };
    let mut full = MultiValue::from_iter([Value::UserData(ud.clone())]);
    full.extend(args);
    files::call_reporting(lua, function, full).ok()
}

/// `InvalidateInternalEntityCache`, which the game's Lua (`entity_iter.lua`) wants called
/// before `OnEntityCreated` and after `EntityRemoved`.
fn invalidate_cache(lua: &Lua, player: bool) {
    if let Ok(Some(f)) = lua.globals().get::<Option<Function>>("InvalidateInternalEntityCache") {
        let _ = files::call_reporting(lua, f, MultiValue::from_iter([Value::Boolean(player)]));
    }
}

/// The first index of entities that are not players: after the world and the player slots
/// (`game.MaxPlayers()`).
fn first_index(lua: &Lua) -> u32 {
    crate::player::max_players(lua) + 1
}

/// Makes an entity of a class, as `ents.Create` does; none when the class is unknown (or every
/// index is used). The entities of a map pass their map creation ID and are made whatever
/// their class.
pub(crate) fn create(lua: &Lua, class: &str, map_creation_id: Option<i32>) -> mlua::Result<Option<EntityHandle>> {
    let script = script_of(lua, class)?;
    let kind = match script {
        Some((kind, _)) => kind,
        None if map_creation_id.is_some() || ENGINE_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class)) => engine_kind(class),
        None => {
            host(lua).print(&format!("Attempted to create unknown entity type {class}!\n"));
            return Ok(None);
        }
    };
    let handle = {
        let host = host(lua);
        let mut ent = Ent::new(class, kind, host.entities.next_creation_id(), host.clock.cur_time.get());
        ent.scripted = script.is_some();
        ent.map_creation_id = map_creation_id;
        match host.entities.insert(ent, first_index(lua)) {
            Some(handle) => handle,
            None => {
                host.print(&format!("ERROR: no free entity slots for {class}!\n"));
                return Ok(None);
            }
        }
    };
    if let Some((_, script)) = script {
        set_up_script(lua, handle, class, script)?;
    }
    if kind == Kind::Weapon {
        crate::weapon::setup(lua, handle);
    }
    announce(lua, handle);
    Ok(Some(handle))
}

/// Gives a scripted entity its class's table (`scripted_ents.Get`, `weapons.Get`) and its
/// data tables (`SetupDataTables`).
fn set_up_script(lua: &Lua, handle: EntityHandle, class: &str, script: Script) -> mlua::Result<()> {
    let ud = userdata(lua, handle).expect("a valid entity");
    let table = entity::table_of(&ud)?.expect("every entity has a table");
    let module = if script == Script::Sent { "scripted_ents" } else { "weapons" };
    let get: Function = lua.globals().get::<Table>(module)?.get("Get")?;
    let _ = files::call_reporting(lua, get, MultiValue::from_iter([Value::String(lua.create_string(class)?), Value::Table(table.clone())]));
    table.raw_set("Entity", &ud)?;
    if script == Script::Swep {
        table.raw_set("Weapon", &ud)?;
    }
    call_method(lua, &ud, "InstallDataTable", MultiValue::new());
    call_method(lua, &ud, "SetupDataTables", MultiValue::new());
    Ok(())
}

/// A client receives an entity of the server's (put in place already, its variables not yet):
/// a scripted one takes its class's table, as on the server.
pub(crate) fn received(lua: &Lua, handle: EntityHandle) -> mlua::Result<()> {
    let Some((class, scripted)) = host(lua).entities.get(handle).map(|ent| (ent.class.clone(), ent.scripted)) else { return Ok(()) };
    if scripted && let Some((_, script)) = script_of(lua, &class)? {
        set_up_script(lua, handle, &class, script)?;
    }
    Ok(())
}

/// A received entity comes into being on the client: `OnEntityCreated`, then a scripted
/// entity's `Initialize`.
pub(crate) fn arrived(lua: &Lua, handle: EntityHandle) {
    announce(lua, handle);
    let scripted = host(lua).entities.get(handle).is_some_and(|ent| ent.scripted);
    if scripted && let Some(ud) = userdata(lua, handle) {
        call_method(lua, &ud, "Initialize", MultiValue::new());
    }
    if let Some(ud) = userdata(lua, handle) {
        crate::call_hook(lua, "NetworkEntityCreated", MultiValue::from_iter([Value::UserData(ud)]));
    }
}

/// Makes an entity of one of the engine's own classes that scripts do not make themselves
/// (`predicted_viewmodel`, `hl2mp_ragdoll`).
pub(crate) fn create_engine(lua: &Lua, class: &str, kind: Kind) -> mlua::Result<Option<EntityHandle>> {
    let handle = {
        let host = host(lua);
        let ent = Ent::new(class, kind, host.entities.next_creation_id(), host.clock.cur_time.get());
        match host.entities.insert(ent, first_index(lua)) {
            Some(handle) => handle,
            None => return Ok(None),
        }
    };
    host(lua).entities.with_mut(handle, |ent| ent.spawned = true);
    announce(lua, handle);
    Ok(Some(handle))
}

/// Tells the game's Lua that an entity was made: `OnEntityCreated`.
pub(crate) fn announce(lua: &Lua, handle: EntityHandle) {
    let Some(ud) = userdata(lua, handle) else { return };
    invalidate_cache(lua, handle.kind == Kind::Player);
    crate::call_hook(lua, "OnEntityCreated", MultiValue::from_iter([Value::UserData(ud)]));
}

const SOLID_VPHYSICS: i64 = 6;
const MOVETYPE_VPHYSICS: i64 = 6;

/// Whether an engine class is a prop of physics: `prop_physics` and its kin
/// (`prop_physics_multiplayer`, `_override`, `_respawnable`).
fn is_physics_prop(class: &str) -> bool {
    class.to_ascii_lowercase().starts_with("prop_physics")
}

/// `ent:Spawn()`: a SENT or SWEP runs its `Initialize`; a prop of physics takes its body, a
/// `prop_ragdoll` its parts ([`crate::ragdoll`]); a player spawns again.
pub(crate) fn spawn(lua: &Lua, handle: EntityHandle) {
    if handle.kind == Kind::Player {
        crate::player::spawn(lua, handle);
        return;
    }
    let now = host(lua).clock.cur_time.get();
    let Some((scripted, prop, ragdoll, flags)) = host(lua).entities.with_mut(handle, |ent| {
        ent.spawned = true;
        ent.next_think = now;
        let engine = !ent.scripted;
        (ent.scripted, engine && is_physics_prop(&ent.class), engine && ent.class.eq_ignore_ascii_case("prop_ragdoll"), ent.spawn_flags)
    }) else {
        return;
    };
    // the engine's props take their bodies when they spawn: frozen when the map says "motion
    // disabled", asleep when it says "start asleep" - 8 and 1 for a prop, 16384 and 65536
    // for a ragdoll, whose "debris" (4, set on most in maps) players and debris go through
    if prop || ragdoll {
        let (frozen, asleep) = if ragdoll { (16384, 65536) } else { (8, 1) };
        host(lua).entities.with_mut(handle, |ent| {
            ent.solid = SOLID_VPHYSICS;
            ent.move_type = MOVETYPE_VPHYSICS;
        });
        let made = if ragdoll { crate::ragdoll::init(lua, handle, Vector::default()) } else { crate::physics::init_model(lua, handle) };
        if made {
            if flags & frozen != 0 {
                crate::physics::set_motion(lua, handle, false);
            } else if flags & asleep != 0 {
                crate::physics::sleep(lua, handle);
            }
        }
        if ragdoll && flags & 4 != 0 {
            host(lua).entities.with_mut(handle, |ent| ent.collision_group = crate::trace::group::DEBRIS);
        }
    }
    if !scripted {
        crate::npc::spawned(lua, handle);
        crate::anim::spawned(lua, handle);
        crate::logic::spawned(lua, handle);
        crate::triggers::spawned(lua, handle);
        crate::movers::spawned(lua, handle);
        crate::items::spawned(lua, handle);
        crate::breakable::spawned(lua, handle);
    }
    if scripted && let Some(ud) = userdata(lua, handle) {
        call_method(lua, &ud, "Initialize", MultiValue::new());
    }
}

/// `ent:Activate()`, after `Spawn`: a constraint takes hold ([`crate::constraint`]).
pub(crate) fn activate(lua: &Lua, handle: EntityHandle) {
    crate::constraint::activate(lua, handle);
}

/// `ent:Remove()`: `OnRemove` and the `EntityRemoved` hook now, the entities it was to take
/// with it (`DeleteOnRemove`) go too, and it is taken out of its parent's children and its
/// children out of it - and out of any player's hands; the entity itself goes at the end of
/// the frame. The world stays.
pub(crate) fn remove(lua: &Lua, handle: EntityHandle) {
    if handle.index == Some(0) {
        return;
    }
    let Some((first, scripted, kind)) = host(lua).entities.with_mut(handle, |ent| {
        let first = !ent.removed;
        ent.removed = true;
        (first, ent.scripted, ent.kind)
    }) else {
        return;
    };
    if !first {
        return;
    }
    let Some(ud) = userdata(lua, handle) else { return };
    if scripted {
        call_method(lua, &ud, "OnRemove", MultiValue::new());
    }
    crate::call_hook(lua, "EntityRemoved", MultiValue::from_iter([Value::UserData(ud.clone())]));
    invalidate_cache(lua, kind == Kind::Player);
    crate::player::forget(lua, handle);
    crate::constraint::removed(lua, handle);
    crate::physics::destroy(lua, handle);
    crate::npc::removed(lua, handle);
    if let Ok(Some(list)) = ud.named_user_value::<Option<Table>>("delete-on-remove") {
        for other in list.sequence_values::<Value>().flatten() {
            if let Some(other) = entity::handle_of(Some(&other)) {
                remove(lua, other);
            }
        }
    }
    // it leaves its parent, and its children stay where they are, without it
    entity::set_parent(lua, handle, EntityHandle::NULL);
    let children: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|ent| ent.parent == handle)).collect()
    };
    for child in children {
        entity::set_parent(lua, child, EntityHandle::NULL);
    }
}

/// The end of a frame: entities that were removed go.
pub(crate) fn collect(lua: &Lua) {
    let gone: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|ent| ent.removed)).collect()
    };
    for handle in gone {
        let _ = entity::forget(lua, handle);
        host(lua).entities.free(handle);
    }
}

/// One tick: the SENTs whose time has come think. A SWEP thinks in a player's hands
/// ([`crate::weapon::post_frame`]).
pub(crate) fn think(lua: &Lua) {
    let now = host(lua).clock.cur_time.get();
    let due: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|&h| h.kind != Kind::Weapon)
            .filter(|&h| entities.get(h).is_some_and(|ent| ent.scripted && ent.spawned && !ent.removed && ent.next_think <= now))
            .collect()
    };
    for handle in due {
        let Some(ud) = userdata(lua, handle) else { continue };
        host(lua).entities.with_mut(handle, |ent| ent.think_request = None);
        let result = call_method(lua, &ud, "Think", MultiValue::new());
        let again = matches!(result.as_ref().and_then(|r| r.front()), Some(Value::Boolean(true)));
        host(lua).entities.with_mut(handle, |ent| {
            let request = ent.think_request.take();
            ent.next_think = if again { request.unwrap_or(now) } else { now + DEFAULT_THINK };
        });
    }
}

/// An entity's model, where it is drawn: what [`crate::Runtime::drawn`] lists.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    pub entity: EntityHandle,
    /// The model's path in the game's files: `models/props_c17/oildrum001.mdl`.
    pub model: String,
    pub skin: i64,
    pub origin: Vector,
    pub angles: Angle,
    pub scale: f32,
    /// What moves the vertices of the model to where it is posed now, in its own space
    /// ([`gmod_model::Rig::skinning`]); empty for a model drawn as it was made.
    pub bones: Vec<[[f32; 4]; 3]>,
}

/// `RENDERMODE_NONE`: not drawn at all.
const RENDERMODE_NONE: i64 = 10;
/// `EF_NODRAW`.
const EF_NODRAW: i64 = 32;

/// The models of entities as `viewer` sees them, through its own eyes: not the viewer itself
/// nor what it carries (its weapon, its view model and the hands on it), not what is hidden,
/// not brush entities (the map draws those) and not a corpse that could not become a ragdoll.
pub(crate) fn drawn(lua: &Lua, viewer: EntityHandle) -> Vec<Drawn> {
    let entities = &host(lua).entities;
    // the viewer, or what goes with its eyes, all the way up the parents
    let carried = |mut handle: EntityHandle| {
        for _ in 0..16 {
            let Some(ent) = entities.get(handle) else { return false };
            if handle == viewer || matches!(ent.class.as_str(), "predicted_viewmodel" | "viewmodel") {
                return true;
            }
            handle = ent.parent;
        }
        false
    };
    entities
        .handles()
        .into_iter()
        .filter(|&handle| !carried(handle))
        .filter_map(|handle| {
            let (model, skin) = {
                let ent = entities.get(handle)?;
                let hidden =
                    ent.removed || !ent.spawned || ent.no_draw || ent.effects & EF_NODRAW != 0 || ent.render_mode == RENDERMODE_NONE;
                let studio = ent.model.len() > 4 && ent.model[ent.model.len() - 4..].eq_ignore_ascii_case(".mdl");
                // the brushes that may move are drawn where their entities are; the others are the map's
                let moving_brush = crate::movers::moving_brush(&ent).is_some();
                if hidden
                    || !(studio || moving_brush)
                    || (ent.class == "hl2mp_ragdoll" && ent.physics.is_none() && ent.ragdoll_pose.is_empty())
                {
                    return None;
                }
                (ent.model.clone(), ent.skin)
            };
            // a merged model is drawn where its parent is, a player's turned as its legs are
            let (origin, angles, scale) = crate::anim::drawn_at(lua, handle)?;
            let angles = crate::player::state(lua, handle, |_, p| p.render_angles).unwrap_or(angles);
            Some(Drawn { entity: handle, model, skin, origin, angles, scale, bones: crate::anim::skinning(lua, handle) })
        })
        .collect()
}

/// Parses `1 2 3` as the engine does: what is missing is 0.
pub(crate) fn three(text: &str) -> [f32; 3] {
    let mut parts = text.split_whitespace().map(|p| crate::convar::atof(p) as f32);
    [parts.next().unwrap_or(0.0), parts.next().unwrap_or(0.0), parts.next().unwrap_or(0.0)]
}

/// A key-value for an entity, from the map or `SetKeyValue`: the `EntityKeyValue` hook may
/// change the value, a SENT's `KeyValue` may take it (returning true), and what the engine
/// knows (`origin`, `angles`, `targetname`, `model` ...) takes effect. Every key is kept.
pub(crate) fn key_value(lua: &Lua, handle: EntityHandle, key: &str, value: &str) {
    let Some(ud) = userdata(lua, handle) else { return };
    let strings = |value: &str| -> mlua::Result<[Value; 2]> {
        Ok([Value::String(lua.create_string(key)?), Value::String(lua.create_string(value)?)])
    };
    let Ok([k, v]) = strings(value) else { return };
    let changed = crate::call_hook(lua, "EntityKeyValue", MultiValue::from_iter([Value::UserData(ud.clone()), k, v]));
    let value = match changed.front().and_then(|v| lua.coerce_string(v.clone()).ok().flatten()) {
        Some(new) => new.to_string_lossy(),
        None => value.to_string(),
    };
    let scripted = host(lua).entities.get(handle).is_some_and(|ent| ent.scripted);
    if scripted {
        let Ok(args) = strings(&value) else { return };
        let taken = call_method(lua, &ud, "KeyValue", MultiValue::from_iter(args));
        if matches!(taken.as_ref().and_then(|r| r.front()), Some(Value::Boolean(true))) {
            return;
        }
    }
    apply_key_value(lua, handle, key, &value);
}

fn apply_key_value(lua: &Lua, handle: EntityHandle, key: &str, value: &str) {
    // a connection of an output is kept as one; an entity may have many of one name
    if crate::outputs::add(lua, handle, key, value) {
        return;
    }
    let lower = key.to_ascii_lowercase();
    let number = || crate::convar::atof(value);
    match lower.as_str() {
        "classname" => return,
        "origin" => {
            let [x, y, z] = three(value);
            entity::set_world_pos(lua, handle, Vector::new(x, y, z));
        }
        "angles" => {
            let [p, y, r] = three(value);
            entity::set_world_angles(lua, handle, Angle::new(p, y, r));
        }
        // a yaw alone; -1 and -2 point up and down
        "angle" => {
            let yaw = number() as f32;
            let angles = match yaw as i32 {
                -1 => Angle::new(-90.0, 0.0, 0.0),
                -2 => Angle::new(90.0, 0.0, 0.0),
                _ => Angle::new(0.0, yaw, 0.0),
            };
            entity::set_world_angles(lua, handle, angles);
        }
        "model" => entity::set_model(lua, handle, value),
        _ => {}
    }
    host(lua).entities.with_mut(handle, |ent| {
        match lower.as_str() {
            "targetname" => ent.name = value.to_string(),
            "skin" => ent.skin = number() as i64,
            "spawnflags" => ent.spawn_flags = number() as i64,
            "health" => ent.health = number() as i64,
            "max_health" => ent.max_health = number() as i64,
            "rendercolor" => {
                let [r, g, b] = three(value);
                ent.color = [r as u8, g as u8, b as u8, ent.color[3]];
            }
            "renderamt" => ent.color[3] = number().clamp(0.0, 255.0) as u8,
            "rendermode" => ent.render_mode = number() as i64,
            "renderfx" => ent.render_fx = number() as i64,
            "modelscale" => ent.model_scale = number() as f32,
            _ => {}
        }
        match ent.keyvalues.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            Some(pair) => pair.1 = value.to_string(),
            None => ent.keyvalues.push((key.to_string(), value.to_string())),
        }
    });
}

/// Loads a map's entities as the server does when the map starts: the world takes the
/// `worldspawn` key-values, every other entity is made with its key-values, parents are set
/// (`parentname`), all are spawned, parents before their children, then activated, the
/// `logic_auto`s fire, and the `InitPostEntity` hook runs.
pub(crate) fn spawn_map(lua: &Lua, map: &Map) {
    {
        let host = host(lua);
        *host.game.map.borrow_mut() = map.name.clone();
        *host.models.brush.borrow_mut() =
            map.brush_models.iter().map(|(lo, hi)| (Vector::new(lo[0], lo[1], lo[2]), Vector::new(hi[0], hi[1], hi[2]))).collect();
        *host.world.collision.borrow_mut() = map.collision.clone();
        *host.world.textures.borrow_mut() = map.textures.clone();
    }
    let world = EntityHandle::WORLD;
    // a client has the world to trace against; the entities come from the server
    if host(lua).realm != Realm::Server {
        host(lua).entities.with_mut(world, |ent| ent.model = format!("maps/{}.bsp", map.name));
        crate::call_hook(lua, "InitPostEntity", MultiValue::new());
        return;
    }
    crate::physics::load_world(lua, map.collision.as_deref());
    crate::movers::clear(lua);
    let world_bounds = model_bounds(lua, "*0").unwrap_or_default();
    host(lua).entities.with_mut(world, |ent| {
        ent.model = format!("maps/{}.bsp", map.name);
        (ent.mins, ent.maxs) = world_bounds;
    });

    let mut made = Vec::new();
    for (i, pairs) in map.entities.iter().enumerate() {
        let Some((_, class)) = pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case("classname")) else { continue };
        let id = FIRST_MAP_CREATION_ID + i as i32;
        let handle = if class.eq_ignore_ascii_case("worldspawn") {
            host(lua).entities.with_mut(world, |ent| ent.map_creation_id = Some(id));
            world
        } else {
            match create(lua, class, Some(id)) {
                Ok(Some(handle)) => handle,
                Ok(None) => continue,
                Err(e) => {
                    host(lua).report(ScriptError::from_lua(&e));
                    continue;
                }
            }
        };
        for (key, value) in pairs {
            if handle == world && key.eq_ignore_ascii_case("model") {
                continue;
            }
            key_value(lua, handle, key, value);
        }
        if handle != world {
            made.push(handle);
        }
    }
    // parents first, as the engine spawns a hierarchy
    for &handle in &made {
        let parent = host(lua).entities.get(handle).and_then(|ent| ent.keyvalue("parentname").map(str::to_string));
        if let Some(parent) = parent.filter(|p| !p.is_empty())
            && let Some(found) = find(lua, |ent| names_match(&parent, &ent.name)).into_iter().next()
        {
            entity::set_parent(lua, handle, found);
        }
    }
    let depth = |handle: EntityHandle| {
        let entities = &host(lua).entities;
        let mut depth = 0;
        let mut parent = entities.get(handle).map(|ent| ent.parent);
        while let Some(p) = parent.filter(|p| entities.is_valid(*p) && depth < 32) {
            depth += 1;
            parent = entities.get(p).map(|ent| ent.parent);
        }
        depth
    };
    made.sort_by_key(|&handle| depth(handle));
    for &handle in &made {
        spawn(lua, handle);
    }
    // once all are there: the constraints take hold of what they name
    for &handle in &made {
        activate(lua, handle);
    }
    crate::logic::map_spawned(lua);
    crate::call_hook(lua, "InitPostEntity", MultiValue::new());
}

/// Source's name matching: ASCII case ignored, a trailing `*` matches any end; an empty
/// pattern matches nothing.
fn names_match(pattern: &str, name: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) => name.len() >= prefix.len() && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes()),
        None => pattern.eq_ignore_ascii_case(name),
    }
}

/// The first entity with a name (Source's matching: a trailing `*` matches any end).
pub(crate) fn named(lua: &Lua, name: &str) -> Option<EntityHandle> {
    find(lua, |ent| names_match(name, &ent.name)).into_iter().next()
}

/// The entities with a name (Source's matching), in index order.
pub(crate) fn all_named(lua: &Lua, name: &str) -> Vec<EntityHandle> {
    find(lua, |ent| names_match(name, &ent.name))
}

/// The entities of a class (Source's matching: a trailing `*` matches any end), in index order.
pub(crate) fn all_of_class(lua: &Lua, class: &str) -> Vec<EntityHandle> {
    find(lua, |ent| names_match(class, &ent.class))
}

/// The entities that are not being removed and pass a test, in index order.
fn find(lua: &Lua, test: impl Fn(&Ent) -> bool) -> Vec<EntityHandle> {
    let entities = &host(lua).entities;
    entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|ent| !ent.removed && test(&ent))).collect()
}

/// The entities whose box in the world passes a test (the world itself left out).
fn find_in_space(lua: &Lua, test: impl Fn(Vector, Vector) -> bool) -> Vec<EntityHandle> {
    let entities = &host(lua).entities;
    entities
        .handles()
        .into_iter()
        .filter(|&h| h.index != Some(0))
        .filter(|&h| entities.get(h).is_some_and(|ent| !ent.removed))
        .filter(|&h| entities.world_bounds(h).is_some_and(|(_, _, lo, hi)| test(lo, hi)))
        .collect()
}

fn list(lua: &Lua, handles: Vec<EntityHandle>) -> mlua::Result<Value> {
    let t = lua.create_table_with_capacity(handles.len(), 0)?;
    for handle in handles {
        t.raw_push(entity::push(lua, handle)?)?;
    }
    Ok(Value::Table(t))
}

/// The distance from a point to a box, squared.
fn distance_squared(p: Vector, lo: Vector, hi: Vector) -> f32 {
    let d = |v: f32, lo: f32, hi: f32| {
        if v < lo {
            lo - v
        } else if v > hi {
            v - hi
        } else {
            0.0
        }
    };
    let (x, y, z) = (d(p.x, lo.x, hi.x), d(p.y, lo.y, hi.y), d(p.z, lo.z, hi.z));
    x * x + y * y + z * z
}

/// Whether the segment from `start` to `end` passes through a box.
fn ray_hits(start: Vector, end: Vector, lo: Vector, hi: Vector) -> bool {
    let (mut enter, mut leave) = (0.0f32, 1.0f32);
    for (s, e, lo, hi) in [(start.x, end.x, lo.x, hi.x), (start.y, end.y, lo.y, hi.y), (start.z, end.z, lo.z, hi.z)] {
        let d = e - s;
        if d.abs() < f32::EPSILON {
            if s < lo || s > hi {
                return false;
            }
            continue;
        }
        let (a, b) = ((lo - s) / d, (hi - s) / d);
        enter = enter.max(a.min(b));
        leave = leave.min(a.max(b));
        if enter > leave {
            return false;
        }
    }
    true
}

/// `Entity(index)`: the entity there, or `NULL` for an index that is not one.
fn by_index(a: &Args) -> mlua::Result<Value> {
    let index = a.number(1)?;
    if index < 0.0 || index.fract() != 0.0 || index >= f64::from(entity::MAX_EDICTS) {
        return entity::push(a.lua, EntityHandle::NULL);
    }
    entity::push_index(a.lua, index as u32)
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

pub(crate) fn install(lua: &Lua, realm: Realm) -> mlua::Result<()> {
    let globals = lua.globals();
    def(lua, &globals, "Entity", by_index)?;

    let ents = lua.create_table()?;
    def(lua, &ents, "GetByIndex", by_index)?;
    def(lua, &ents, "GetAll", |a| list(a.lua, find(a.lua, |_| true)))?;
    def(lua, &ents, "GetCount", |a| {
        let include_removed = a.boolean(1);
        let entities = &host(a.lua).entities;
        let count = entities.handles().into_iter().filter(|&h| entities.get(h).is_some_and(|ent| include_removed || !ent.removed)).count();
        Ok(Value::Integer(count as i64))
    })?;
    def(lua, &ents, "GetEdictCount", |a| Ok(Value::Integer(host(a.lua).entities.handles().len() as i64)))?;
    def(lua, &ents, "FindByClass", |a| {
        let pattern = a.string(1)?;
        list(a.lua, find(a.lua, |ent| names_match(&pattern, &ent.class)))
    })?;
    def(lua, &ents, "FindByName", |a| {
        let pattern = a.string(1)?;
        list(a.lua, find(a.lua, |ent| !ent.name.is_empty() && names_match(&pattern, &ent.name)))
    })?;
    def(lua, &ents, "FindByModel", |a| {
        let model = a.string(1)?;
        list(a.lua, find(a.lua, |ent| names_match(&model, &ent.model)))
    })?;
    def(lua, &ents, "FindInSphere", |a| {
        let (center, radius) = (a.userdata::<Vector>(1, "Vector")?, a.number(2)? as f32);
        list(a.lua, find_in_space(a.lua, |lo, hi| distance_squared(center, lo, hi) <= radius * radius))
    })?;
    def(lua, &ents, "FindInBox", |a| {
        let (p, q) = (a.userdata::<Vector>(1, "Vector")?, a.userdata::<Vector>(2, "Vector")?);
        let (blo, bhi) = (Vector::new(p.x.min(q.x), p.y.min(q.y), p.z.min(q.z)), Vector::new(p.x.max(q.x), p.y.max(q.y), p.z.max(q.z)));
        list(
            a.lua,
            find_in_space(a.lua, |lo, hi| {
                lo.x <= bhi.x && hi.x >= blo.x && lo.y <= bhi.y && hi.y >= blo.y && lo.z <= bhi.z && hi.z >= blo.z
            }),
        )
    })?;
    def(lua, &ents, "FindInCone", |a| {
        let origin = a.userdata::<Vector>(1, "Vector")?;
        let normal = a.userdata::<Vector>(2, "Vector")?.normalized();
        let (range, cos) = (a.number(3)? as f32, a.number(4)? as f32);
        list(
            a.lua,
            find_in_space(a.lua, |lo, hi| {
                if distance_squared(origin, lo, hi) > range * range {
                    return false;
                }
                let to = (lo + hi) * 0.5 - origin;
                to.length() < f32::EPSILON || to.normalized().dot(normal) >= cos
            }),
        )
    })?;
    def(lua, &ents, "FindAlongRay", |a| {
        let (start, end) = (a.userdata::<Vector>(1, "Vector")?, a.userdata::<Vector>(2, "Vector")?);
        let mins = if a.get(3).is_some_and(|v| !v.is_nil()) { a.userdata::<Vector>(3, "Vector")? } else { Vector::default() };
        let maxs = if a.get(4).is_some_and(|v| !v.is_nil()) { a.userdata::<Vector>(4, "Vector")? } else { Vector::default() };
        list(a.lua, find_in_space(a.lua, |lo, hi| ray_hits(start, end, lo - maxs, hi - mins)))
    })?;
    def(lua, &ents, "GetMapCreatedEntity", |a| {
        let id = a.number(1)? as i32;
        let found = find(a.lua, |ent| ent.map_creation_id == Some(id)).into_iter().next().unwrap_or(EntityHandle::NULL);
        entity::push(a.lua, found)
    })?;
    if realm == Realm::Server {
        def(lua, &ents, "Create", |a| {
            let class = a.string(1)?;
            match create(a.lua, &class, None)? {
                Some(handle) => entity::push(a.lua, handle),
                None => entity::push(a.lua, EntityHandle::NULL),
            }
        })?;
    }
    globals.raw_set("ents", ents)?;
    // the networked globals are the world's networked variables: they reach the clients as
    // its variables do
    lua.load(GLOBALS).set_name("=globals").exec()?;
    Ok(())
}

/// `SetGlobalString`, `GetGlobal2Int` and the rest, on the world's `SetNW*`, `SetNW2*`.
const GLOBALS: &str = r#"
    local meta = FindMetaTable("Entity")
    for _, kind in ipairs({ "String", "Int", "Float", "Bool", "Vector", "Angle", "Entity" }) do
        for _, two in ipairs({ "", "2" }) do
            local set, get = meta["SetNW" .. two .. kind], meta["GetNW" .. two .. kind]
            _G["SetGlobal" .. two .. kind] = function(key, value) set(Entity(0), key, value) end
            _G["GetGlobal" .. two .. kind] = function(key, default) return get(Entity(0), key, default) end
        end
    end
    for _, name in ipairs({ "SetGlobalVar", "SetGlobal2Var" }) do
        _G[name] = function(key, value) meta.SetNW2Var(Entity(0), key, value) end
    end
    for _, name in ipairs({ "GetGlobalVar", "GetGlobal2Var" }) do
        _G[name] = function(key, default) return meta.GetNW2Var(Entity(0), key, default) end
    end
"#;

/// The types of networked variables: how a value is taken and what is there by default.
#[derive(Clone, Copy)]
enum VarType {
    Int,
    Float,
    Bool,
    String,
    Entity,
    Vector,
    Angle,
}

fn var_default(lua: &Lua, ty: VarType) -> mlua::Result<Value> {
    Ok(match ty {
        VarType::Int => Value::Integer(0),
        VarType::Float => Value::Number(0.0),
        VarType::Bool => Value::Boolean(false),
        VarType::String => Value::String(lua.create_string("")?),
        VarType::Entity => entity::push(lua, EntityHandle::NULL)?,
        VarType::Vector => entity::vector(lua, Vector::default())?,
        VarType::Angle => entity::angle(lua, Angle::default())?,
    })
}

fn var_value(a: &Args, position: usize, ty: VarType) -> mlua::Result<Value> {
    let value = a.get(position).cloned().unwrap_or(Value::Nil);
    Ok(match ty {
        VarType::Int => Value::Integer(a.lua.coerce_number(value)?.unwrap_or(0.0) as i64),
        VarType::Float => Value::Number(a.lua.coerce_number(value)?.unwrap_or(0.0)),
        VarType::Bool => Value::Boolean(!matches!(value, Value::Nil | Value::Boolean(false))),
        VarType::String => Value::String(a.lua.coerce_string(value)?.unwrap_or(a.lua.create_string("")?)),
        VarType::Entity => match entity::handle_of(Some(&value)) {
            Some(handle) => entity::push(a.lua, handle)?,
            None => entity::push(a.lua, EntityHandle::NULL)?,
        },
        VarType::Vector => entity::vector(a.lua, a.userdata::<Vector>(position, "Vector")?)?,
        VarType::Angle => entity::angle(a.lua, a.userdata::<Angle>(position, "Angle")?)?,
    })
}

/// The entity's table of variables of one kind (`nw`, `nw2`, `dt`).
fn vars(a: &Args, store: &str) -> mlua::Result<Table> {
    entity::this(a, 1)?;
    match a.get(1) {
        Some(Value::UserData(ud)) => entity::store_of(a.lua, ud, store),
        _ => Err(a.bad(1, "Entity")),
    }
}

/// `SetNWInt(key, value)` and the like.
fn set_var(a: &Args, store: &str, ty: VarType) -> mlua::Result<Value> {
    let vars = vars(a, store)?;
    let key = a.string(2)?;
    vars.raw_set(key, var_value(a, 3, ty)?)?;
    Ok(Value::Nil)
}

/// `GetNWInt(key, default)` and the like.
fn get_var(a: &Args, store: &str, ty: VarType) -> mlua::Result<Value> {
    let vars = vars(a, store)?;
    let key = a.string(2)?;
    match vars.raw_get::<Value>(key)? {
        Value::Nil => match a.get(3) {
            Some(value) if !value.is_nil() => Ok(value.clone()),
            _ => var_default(a.lua, ty),
        },
        value => Ok(value),
    }
}

/// `SetDTInt(index, value)` and the like: the slots `NetworkVar` uses.
fn set_dt(a: &Args, ty: VarType, name: &str) -> mlua::Result<Value> {
    let vars = vars(a, "dt")?;
    let index = a.number(2)? as i64;
    vars.raw_set(format!("{name}{index}"), var_value(a, 3, ty)?)?;
    Ok(Value::Nil)
}

fn get_dt(a: &Args, ty: VarType, name: &str) -> mlua::Result<Value> {
    let vars = vars(a, "dt")?;
    let index = a.number(2)? as i64;
    match vars.raw_get::<Value>(format!("{name}{index}"))? {
        Value::Nil => var_default(a.lua, ty),
        value => Ok(value),
    }
}

fn copy(lua: &Lua, table: &Table) -> mlua::Result<Value> {
    let out = lua.create_table()?;
    for pair in table.pairs::<Value, Value>() {
        let (k, v) = pair?;
        out.raw_set(k, v)?;
    }
    Ok(Value::Table(out))
}

macro_rules! networked {
    ($def:expr, $ty:ident) => {
        $def(concat!("SetNW", stringify!($ty)), |a| set_var(a, "nw", VarType::$ty))?;
        $def(concat!("GetNW", stringify!($ty)), |a| get_var(a, "nw", VarType::$ty))?;
        $def(concat!("SetNetworked", stringify!($ty)), |a| set_var(a, "nw", VarType::$ty))?;
        $def(concat!("GetNetworked", stringify!($ty)), |a| get_var(a, "nw", VarType::$ty))?;
        $def(concat!("SetNW2", stringify!($ty)), |a| set_var(a, "nw2", VarType::$ty))?;
        $def(concat!("GetNW2", stringify!($ty)), |a| get_var(a, "nw2", VarType::$ty))?;
        $def(concat!("SetDT", stringify!($ty)), |a| set_dt(a, VarType::$ty, stringify!($ty)))?;
        $def(concat!("GetDT", stringify!($ty)), |a| get_dt(a, VarType::$ty, stringify!($ty)))?;
    };
}

/// The variables the engine keeps with an entity and (from stage 6) sends to clients: NW,
/// NW2 and the data table slots of `NetworkVar`.
pub(crate) fn install_variables(def: &entity::Def) -> mlua::Result<()> {
    networked!(def, Int);
    networked!(def, Float);
    networked!(def, Bool);
    networked!(def, String);
    networked!(def, Entity);
    networked!(def, Vector);
    networked!(def, Angle);
    def("SetNW2Var", |a| {
        let vars = vars(a, "nw2")?;
        vars.raw_set(a.string(2)?, a.get(3).cloned().unwrap_or(Value::Nil))?;
        Ok(Value::Nil)
    })?;
    def("GetNW2Var", |a| {
        let vars = vars(a, "nw2")?;
        match vars.raw_get::<Value>(a.string(2)?)? {
            Value::Nil => Ok(a.get(3).cloned().unwrap_or(Value::Nil)),
            value => Ok(value),
        }
    })?;
    def("GetNWVarTable", |a| copy(a.lua, &vars(a, "nw")?))?;
    def("GetNW2VarTable", |a| copy(a.lua, &vars(a, "nw2")?))?;
    Ok(())
}
