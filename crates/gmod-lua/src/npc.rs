//! NPCs: the engine's own (`npc_citizen`, `npc_zombie`, `npc_combine_s` ...) as sandbox's
//! spawn menu makes them (`gmod_spawnnpc`: `ents.Create`, key-values, `Spawn`), and the `NPC`
//! metatable. What they do by themselves - moving, fighting - is [`crate::ai`]'s.
//!
//! Spawned, an NPC takes what its class has in HL2 unless it was given otherwise: its model
//! (a citizen's head and clothes by `citizentype` and its spawn flags), its health (the skill
//! ConVar `sk_<npc>_health` when one is set, else HL2's default), its hull - a solid box,
//! human-sized for people and zombies, smaller for headcrabs and birds - and it moves by steps
//! on the ground (scanners and manhacks fly, turrets and barnacles stay put), dropped onto the
//! floor below; one without a box (`npc_enemyfinder`) is a point nothing runs into. A side
//! given before `Spawn` (`SetNPCClass`) is kept. It plays its idle (`ACT_IDLE`) and takes the
//! weapon of `additionalequipment`, held in its hand as a player's is (bone-merged, drawn). A
//! map's template NPCs (`SF_NPC_TEMPLATE`) go as the map loads. Without an enemy, one that
//! hates a player or an NPC it can see within its look distance takes the nearest (it looks
//! five times a second), and remembers where it sees it. As in GMod, `ai_disabled` stops all
//! it does, and players with `FL_NOTARGET`, or all of them with `ai_ignoreplayers`, are not
//! taken as enemies - when seen or when they shoot - and one that was is let go.
//!
//! Damage ([`crate::damage`]) hurts it: `OnDamaged` (and `OnDamagedByPlayer`) fire,
//! `OnHalfHealth` once it is down to half, and it takes a hated attacker as its enemy
//! (bullets ask `ScaleNPCDamage` first). At no health it dies: `OnNPCKilled`, `OnDeath`, its
//! weapon falls (unless `SF_NPC_NO_WEAPON_DROP`), a health vial with `SF_NPC_DROP_HEALTHKIT`,
//! and the NPC goes, leaving a corpse: a ragdoll of its model posed as it was drawn and pushed
//! by the killing blow ([`crate::ragdoll`]). As in GMod the corpse is the client's by default -
//! it lies on the world and what does not move, nothing traces or walks into it - and a
//! `prop_ragdoll` of the server's with `ai_serverragdolls 1` (and `CreateEntityRagdoll`). A
//! model that cannot be a ragdoll leaves none; with `SF_NPC_FADE_CORPSE`, which sandbox
//! always sets, the corpse fades a while later, and of the client's there are 32 at most, the
//! oldest going first. A removed NPC takes the weapon in its hand with it - on the server: a
//! client has what the server sends.
//!
//! The `NPC` methods keep what they are told - enemies and what it remembers of them,
//! relationships (`AddRelationship`, `AddEntityRelationship`, `Disposition`, by default those
//! of HL2's sides: rebels like the player, the Combine, zombies and antlions hate them and
//! each other), schedules, state, capabilities, squads (`ai.GetSquadMembers`), activities,
//! weapon proficiency; goals and ways go to [`crate::ai`]. Sounds and scenes are quiet.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::damage::DamageInfo;
use crate::entity::{self, Def, Ent, EntityHandle, Kind};
use crate::meta::{self, type_id};
use crate::vector::{Angle, Vector};
use crate::{host, timer};

const SOLID_NONE: i64 = 0;
const SOLID_BBOX: i64 = 2;
const MOVETYPE_NONE: i64 = 0;
const MOVETYPE_STEP: i64 = 3;
const MOVETYPE_FLY: i64 = 4;
const EF_BONEMERGE: i64 = 1;
const EF_NODRAW: i64 = 32;
const FL_NPC: i64 = 16384;
const FL_NOTARGET: i64 = 65536;
const RENDERMODE_TRANSALPHA: i64 = 4;

const ACT_IDLE: i64 = 1;
const ACT_INVALID: i64 = -1;
const SCHED_NONE: i64 = 0;
const SCHED_IDLE_STAND: i64 = 1;
const NPC_STATE_IDLE: i64 = 1;
const NPC_STATE_ALERT: i64 = 2;
const NPC_STATE_COMBAT: i64 = 3;
const NPC_STATE_DEAD: i64 = 7;
const WEAPON_PROFICIENCY_AVERAGE: i64 = 1;

const SF_NPC_DROP_HEALTHKIT: i64 = 8;
const SF_NPC_FADE_CORPSE: i64 = 512;
const SF_NPC_TEMPLATE: i64 = 2048;
const SF_NPC_NO_WEAPON_DROP: i64 = 8192;
const SF_FLOOR_TURRET_CITIZEN: i64 = 512;
const SF_CITIZEN_MEDIC: i64 = 131072;
const SF_CITIZEN_RANDOM_HEAD_MALE: i64 = 4194304;
const SF_CITIZEN_RANDOM_HEAD_FEMALE: i64 = 8388608;
/// sandbox's own flag of `npc_breen`: on the Combine's side.
const SF_BREEN_GMOD_SPAWNMENU: i64 = 131072;
const SF_ROLLERMINE_HACKED: i64 = 262144;
const SF_MANHACK_HACKED: i64 = 2097152;

/// Dispositions (`D_`).
const D_ER: i64 = 0;
const D_HT: i64 = 1;
const D_FR: i64 = 2;
const D_LI: i64 = 3;
const D_NU: i64 = 4;

/// Classes (`CLASS_`): whose side an NPC is on.
mod class {
    pub const NONE: i64 = 0;
    pub const PLAYER: i64 = 1;
    pub const PLAYER_ALLY: i64 = 2;
    pub const PLAYER_ALLY_VITAL: i64 = 3;
    pub const CITIZEN_PASSIVE: i64 = 7;
    pub const CITIZEN_REBEL: i64 = 8;
    pub const CONSCRIPT: i64 = 11;
    pub const MILITARY: i64 = 15;
    pub const ANTLION: i64 = 4;
    pub const BARNACLE: i64 = 5;
    pub const BULLSEYE: i64 = 6;
    pub const COMBINE: i64 = 9;
    pub const COMBINE_GUNSHIP: i64 = 10;
    pub const HEADCRAB: i64 = 12;
    pub const MANHACK: i64 = 13;
    pub const METROPOLICE: i64 = 14;
    pub const SCANNER: i64 = 16;
    pub const STALKER: i64 = 17;
    pub const VORTIGAUNT: i64 = 18;
    pub const ZOMBIE: i64 = 19;
    pub const PROTOSNIPER: i64 = 20;
    pub const EARTH_FAUNA: i64 = 23;
    pub const HACKED_ROLLERMINE: i64 = 24;
    pub const COMBINE_HUNTER: i64 = 25;
}

/// Capabilities (`CAP_`) the engine gives people, and what bites.
const CAPS_HUMAN: i64 = 1 | 64 | 256 | 1024 | 2048 | 4096 | 8192 | 2097152 | 8388608 | 67108864 | 536870912;
const CAPS_MONSTER: i64 = 1 | 524288 | 67108864;
const CAPS_GROUND: i64 = 1;

/// How long a fading corpse lies before it goes, and how long it takes to fade.
const CORPSE_TIME: f64 = 15.0;
const CORPSE_FADE: f64 = 1.0;
/// How many of the client's corpses there are at most, as GMod keeps (`g_ragdoll_maxcount`):
/// past it the oldest goes.
const CORPSE_CAP: usize = 32;
/// How far an NPC sees, by default, and how often it looks for enemies.
const LOOK_DISTANCE: f32 = 2048.0;
const LOOK_INTERVAL: f64 = 0.2;
/// How fast it turns to face its enemy, in degrees a second.
const TURN_SPEED: f32 = 180.0;
/// The class of the corpse that stands in for the client's ragdoll.
const CLIENT_RAGDOLL: &str = "class C_ClientRagdoll";

/// Where an NPC of a class is: on the ground, in the air, or fixed where it was put.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    Ground,
    Air,
    Fixed,
}

/// What the engine gives an NPC of a class: model, health (its skill ConVar and HL2's default
/// at normal skill), hull (`HULL_`, none for the model's own box), side, place, capabilities.
pub(crate) struct Class {
    name: &'static str,
    model: &'static str,
    health: (&'static str, i64),
    hull: Option<i64>,
    side: i64,
    pub(crate) place: Place,
    caps: i64,
}

const fn npc(
    name: &'static str,
    model: &'static str,
    health: (&'static str, i64),
    hull: Option<i64>,
    side: i64,
    place: Place,
    caps: i64,
) -> Class {
    Class { name, model, health, hull, side, place, caps }
}

const HUMAN: Option<i64> = Some(0);
const SMALL_CENTERED: Option<i64> = Some(1);
const WIDE_HUMAN: Option<i64> = Some(2);
const TINY: Option<i64> = Some(3);
const MEDIUM: Option<i64> = Some(5);
const TINY_CENTERED: Option<i64> = Some(6);
const LARGE: Option<i64> = Some(7);
const MEDIUM_TALL: Option<i64> = Some(9);

/// HL2's NPCs, best effort; others are people of no side with their model's box.
#[rustfmt::skip]
const CLASSES: &[Class] = &[
    npc("npc_alyx", "models/alyx.mdl", ("sk_alyx_health", 80), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_barney", "models/barney.mdl", ("sk_barney_health", 35), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_citizen", "", ("sk_citizen_health", 40), HUMAN, class::PLAYER_ALLY, Place::Ground, CAPS_HUMAN),
    npc("npc_kleiner", "models/kleiner.mdl", ("", 100), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_eli", "models/eli.mdl", ("", 100), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_mossman", "models/mossman.mdl", ("", 100), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_magnusson", "models/magnusson.mdl", ("", 100), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_monk", "models/monk.mdl", ("", 100), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_fisherman", "models/lostcoast/fisherman/fisherman.mdl", ("", 100), HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_HUMAN),
    npc("npc_breen", "models/breen.mdl", ("", 100), HUMAN, class::NONE, Place::Ground, CAPS_HUMAN),
    npc("npc_gman", "models/gman.mdl", ("", 100), HUMAN, class::NONE, Place::Ground, CAPS_HUMAN),
    npc("npc_dog", "models/dog.mdl", ("", 100), WIDE_HUMAN, class::PLAYER_ALLY_VITAL, Place::Ground, CAPS_GROUND),
    npc("npc_vortigaunt", "models/vortigaunt.mdl", ("sk_vortigaunt_health", 100), WIDE_HUMAN, class::VORTIGAUNT, Place::Ground, CAPS_MONSTER),
    npc("npc_combine_s", "models/combine_soldier.mdl", ("sk_combine_s_health", 50), HUMAN, class::COMBINE, Place::Ground, CAPS_HUMAN),
    npc("npc_metropolice", "models/police.mdl", ("sk_metropolice_health", 40), HUMAN, class::METROPOLICE, Place::Ground, CAPS_HUMAN),
    npc("npc_stalker", "models/stalker.mdl", ("sk_stalker_health", 50), HUMAN, class::STALKER, Place::Ground, CAPS_GROUND),
    npc("npc_hunter", "models/hunter.mdl", ("sk_hunter_health", 210), MEDIUM_TALL, class::COMBINE_HUNTER, Place::Ground, CAPS_MONSTER),
    npc("npc_strider", "models/combine_strider.mdl", ("sk_strider_health", 350), None, class::COMBINE, Place::Ground, CAPS_GROUND),
    npc("npc_manhack", "models/manhack.mdl", ("sk_manhack_health", 25), TINY_CENTERED, class::MANHACK, Place::Air, CAPS_GROUND),
    npc("npc_cscanner", "models/combine_scanner.mdl", ("sk_scanner_health", 30), TINY_CENTERED, class::SCANNER, Place::Air, CAPS_GROUND),
    npc("npc_clawscanner", "models/shield_scanner.mdl", ("sk_scanner_health", 30), TINY_CENTERED, class::SCANNER, Place::Air, CAPS_GROUND),
    npc("npc_helicopter", "models/combine_helicopter.mdl", ("sk_helicopter_health", 5600), None, class::COMBINE_GUNSHIP, Place::Air, CAPS_GROUND),
    npc("npc_combinegunship", "models/gunship.mdl", ("", 100), None, class::COMBINE_GUNSHIP, Place::Air, CAPS_GROUND),
    npc("npc_combinedropship", "models/combine_dropship.mdl", ("", 100), None, class::COMBINE, Place::Air, CAPS_GROUND),
    npc("npc_rollermine", "models/roller.mdl", ("", 100), SMALL_CENTERED, class::COMBINE, Place::Fixed, CAPS_GROUND),
    npc("npc_turret_floor", "models/combine_turrets/floor_turret.mdl", ("", 100), None, class::COMBINE, Place::Fixed, 0),
    npc("npc_turret_ceiling", "models/combine_turrets/ceiling_turret.mdl", ("", 100), None, class::COMBINE, Place::Fixed, 0),
    npc("npc_combine_camera", "models/combine_camera/combine_camera.mdl", ("", 100), None, class::COMBINE, Place::Fixed, 0),
    npc("npc_sniper", "", ("", 100), None, class::PROTOSNIPER, Place::Fixed, 0),
    npc("npc_zombie", "models/zombie/classic.mdl", ("sk_zombie_health", 50), HUMAN, class::ZOMBIE, Place::Ground, CAPS_MONSTER),
    npc("npc_zombie_torso", "models/zombie/classic_torso.mdl", ("sk_zombie_health", 50), TINY, class::ZOMBIE, Place::Ground, CAPS_MONSTER),
    npc("npc_fastzombie", "models/zombie/fast.mdl", ("sk_fastzombie_health", 50), HUMAN, class::ZOMBIE, Place::Ground, CAPS_MONSTER),
    npc("npc_fastzombie_torso", "models/zombie/fast_torso.mdl", ("sk_fastzombie_health", 50), TINY, class::ZOMBIE, Place::Ground, CAPS_MONSTER),
    npc("npc_poisonzombie", "models/zombie/poison.mdl", ("sk_poisonzombie_health", 175), HUMAN, class::ZOMBIE, Place::Ground, CAPS_MONSTER),
    npc("npc_zombine", "models/zombie/zombie_soldier.mdl", ("sk_zombie_soldier_health", 100), HUMAN, class::ZOMBIE, Place::Ground, CAPS_MONSTER),
    npc("npc_headcrab", "models/headcrabclassic.mdl", ("sk_headcrab_health", 10), TINY, class::HEADCRAB, Place::Ground, CAPS_MONSTER),
    npc("npc_headcrab_fast", "models/headcrab.mdl", ("sk_headcrab_fast_health", 10), TINY, class::HEADCRAB, Place::Ground, CAPS_MONSTER),
    npc("npc_headcrab_black", "models/headcrabblack.mdl", ("sk_headcrab_poison_health", 35), TINY, class::HEADCRAB, Place::Ground, CAPS_MONSTER),
    npc("npc_headcrab_poison", "models/headcrabblack.mdl", ("sk_headcrab_poison_health", 35), TINY, class::HEADCRAB, Place::Ground, CAPS_MONSTER),
    npc("npc_antlion", "models/antlion.mdl", ("sk_antlion_health", 30), MEDIUM, class::ANTLION, Place::Ground, CAPS_MONSTER),
    // its own health is not known here: an antlion's
    npc("npc_antlion_worker", "models/antlion_worker.mdl", ("sk_antlion_health", 30), MEDIUM, class::ANTLION, Place::Ground, CAPS_MONSTER),
    npc("npc_antlionguard", "models/antlion_guard.mdl", ("sk_antlionguard_health", 500), LARGE, class::ANTLION, Place::Ground, CAPS_MONSTER),
    npc("npc_antlion_grub", "models/antlion_grub.mdl", ("", 1), None, class::ANTLION, Place::Fixed, 0),
    npc("npc_barnacle", "models/barnacle.mdl", ("sk_barnacle_health", 35), None, class::BARNACLE, Place::Fixed, 0),
    npc("npc_crow", "models/crow.mdl", ("sk_crow_health", 1), TINY, class::EARTH_FAUNA, Place::Ground, CAPS_GROUND),
    npc("npc_pigeon", "models/pigeon.mdl", ("sk_crow_health", 1), TINY, class::EARTH_FAUNA, Place::Ground, CAPS_GROUND),
    npc("npc_seagull", "models/seagull.mdl", ("sk_crow_health", 1), TINY, class::EARTH_FAUNA, Place::Ground, CAPS_GROUND),
    npc("npc_bullseye", "", ("", 100), None, class::BULLSEYE, Place::Fixed, 0),
];

/// The class's entry, or that of a person of no side.
pub(crate) fn class_of(name: &str) -> &'static Class {
    const OTHER: Class = npc("", "", ("", 100), None, class::NONE, Place::Ground, CAPS_GROUND);
    CLASSES.iter().find(|c| c.name.eq_ignore_ascii_case(name)).unwrap_or(&OTHER)
}

/// The box of a hull (`HULL_`), as the NPC stands.
fn hull_box(hull: i64) -> Option<(Vector, Vector)> {
    let (lo, hi) = match hull {
        0 => ([-13.0, -13.0, 0.0], [13.0, 13.0, 72.0]),
        1 => ([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]),
        2 => ([-15.0, -15.0, 0.0], [15.0, 15.0, 72.0]),
        3 => ([-12.0, -12.0, 0.0], [12.0, 12.0, 24.0]),
        4 => ([-35.0, -35.0, 0.0], [35.0, 35.0, 32.0]),
        5 => ([-16.0, -16.0, 0.0], [16.0, 16.0, 64.0]),
        6 => ([-8.0, -8.0, -4.0], [8.0, 8.0, 4.0]),
        7 => ([-40.0, -40.0, 0.0], [40.0, 40.0, 100.0]),
        8 => ([-38.0, -38.0, -38.0], [38.0, 38.0, 38.0]),
        9 => ([-18.0, -18.0, 0.0], [18.0, 18.0, 100.0]),
        _ => return None,
    };
    Some((Vector::new(lo[0], lo[1], lo[2]), Vector::new(hi[0], hi[1], hi[2])))
}

/// Whom a relationship is with: an entity, or every entity of a class or name.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Entity(EntityHandle),
    Named(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Relationship {
    target: Target,
    disposition: i64,
    priority: i64,
}

/// What an NPC knows of an enemy: where it was last, and when it was first and last seen.
#[derive(Debug, Clone, PartialEq)]
struct Memory {
    enemy: EntityHandle,
    position: Vector,
    first_seen: f64,
    last_seen: f64,
}

/// What the engine keeps for an NPC.
#[derive(Debug, Clone)]
pub struct Npc {
    /// Its side (`Classify`).
    pub(crate) class: i64,
    /// The side was given (`SetNPCClass`): spawning keeps it.
    pub(crate) class_set: bool,
    pub(crate) hull: i64,
    pub(crate) weapon: EntityHandle,
    pub(crate) proficiency: i64,
    pub(crate) enemy: EntityHandle,
    memory: Vec<Memory>,
    pub(crate) target: EntityHandle,
    relationships: Vec<Relationship>,
    pub(crate) schedule: i64,
    pub(crate) state: i64,
    pub(crate) conditions: Vec<i64>,
    pub(crate) capabilities: i64,
    pub(crate) look_distance: f32,
    pub(crate) next_look: f64,
    pub(crate) last_position: Vector,
    pub(crate) movement_activity: i64,
    pub(crate) movement_sequence: i64,
    pub(crate) arrival_activity: i64,
    pub(crate) arrival_sequence: i64,
    pub(crate) squad: String,
    /// `OnHalfHealth` has fired.
    pub(crate) half_health: bool,
    pub(crate) dead: bool,
    /// What it is doing: its schedule's goal and way, its attacks.
    pub(crate) mind: crate::ai::Mind,
}

impl Npc {
    fn new(ent: &Ent) -> Npc {
        let info = class_of(&ent.class);
        Npc {
            class: classify(ent),
            class_set: false,
            hull: info.hull.unwrap_or(0),
            weapon: EntityHandle::NULL,
            proficiency: WEAPON_PROFICIENCY_AVERAGE,
            enemy: EntityHandle::NULL,
            memory: Vec::new(),
            target: EntityHandle::NULL,
            relationships: Vec::new(),
            schedule: SCHED_NONE,
            state: NPC_STATE_IDLE,
            conditions: Vec::new(),
            capabilities: 0,
            look_distance: LOOK_DISTANCE,
            next_look: 0.0,
            last_position: Vector::default(),
            movement_activity: ACT_INVALID,
            movement_sequence: -1,
            arrival_activity: ACT_INVALID,
            arrival_sequence: -1,
            squad: String::new(),
            half_health: false,
            dead: false,
            mind: crate::ai::Mind::default(),
        }
    }
}

/// A corpse the engine looks after: when it goes, if it fades, and whether it is the client's.
struct Corpse {
    body: EntityHandle,
    fades_at: Option<f64>,
    client: bool,
}

/// The corpses that fade and those of the client, oldest first.
#[derive(Default)]
pub(crate) struct Npcs {
    corpses: RefCell<Vec<Corpse>>,
}

/// The side of an NPC of a class, as its spawn flags change it.
fn classify(ent: &Ent) -> i64 {
    let flags = ent.spawn_flags;
    match ent.class.to_ascii_lowercase().as_str() {
        "npc_breen" if flags & SF_BREEN_GMOD_SPAWNMENU != 0 => class::COMBINE,
        "npc_turret_floor" if flags & SF_FLOOR_TURRET_CITIZEN != 0 => class::PLAYER_ALLY,
        "npc_rollermine" if flags & SF_ROLLERMINE_HACKED != 0 => class::HACKED_ROLLERMINE,
        "npc_manhack" if flags & SF_MANHACK_HACKED != 0 => class::PLAYER_ALLY,
        _ => class_of(&ent.class).side,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Rebels,
    Combine,
    Xen,
    Antlions,
    Fauna,
    Neutral,
}

fn side(class: i64) -> Side {
    match class {
        class::PLAYER
        | class::PLAYER_ALLY
        | class::PLAYER_ALLY_VITAL
        | class::CITIZEN_PASSIVE
        | class::CITIZEN_REBEL
        | class::VORTIGAUNT
        | class::HACKED_ROLLERMINE => Side::Rebels,
        class::COMBINE
        | class::COMBINE_GUNSHIP
        | class::CONSCRIPT
        | class::MANHACK
        | class::METROPOLICE
        | class::MILITARY
        | class::SCANNER
        | class::STALKER
        | class::PROTOSNIPER
        | class::COMBINE_HUNTER => Side::Combine,
        class::BARNACLE | class::HEADCRAB | class::ZOMBIE => Side::Xen,
        class::ANTLION => Side::Antlions,
        class::EARTH_FAUNA => Side::Fauna,
        _ => Side::Neutral,
    }
}

/// How one class feels about another when nobody said: those of a side like each other and
/// hate the other sides; birds fear everyone and nobody minds them.
fn default_disposition(me: i64, them: i64) -> i64 {
    match (side(me), side(them)) {
        (Side::Neutral, _) | (_, Side::Neutral) => D_NU,
        (Side::Fauna, Side::Fauna) => D_NU,
        (Side::Fauna, _) => D_FR,
        (_, Side::Fauna) => D_NU,
        (a, b) if a == b => D_LI,
        _ => D_HT,
    }
}

/// Runs `f` on an NPC and what the engine keeps for it, made when first needed.
pub(crate) fn with_npc<R>(lua: &Lua, handle: EntityHandle, f: impl FnOnce(&mut Ent, &mut Npc) -> R) -> Option<R> {
    host(lua)
        .entities
        .with_mut(handle, |ent| {
            if ent.kind != Kind::Npc {
                return None;
            }
            let mut npc = ent.npc.take().unwrap_or_else(|| Box::new(Npc::new(ent)));
            let result = f(ent, &mut npc);
            ent.npc = Some(npc);
            Some(result)
        })
        .flatten()
}

fn value(lua: &Lua, handle: EntityHandle) -> Value {
    entity::push(lua, handle).unwrap_or(Value::Nil)
}

fn hook(lua: &Lua, name: &str, args: impl IntoIterator<Item = Value>) -> MultiValue {
    crate::call_hook(lua, name, MultiValue::from_iter(args))
}

fn number(lua: &Lua, name: &str) -> Option<f64> {
    crate::convar::value(lua, name).map(|v| crate::convar::atof(&v))
}

/// The class of a living thing another may like or hate: players, NPCs, nothing else.
fn class_of_entity(lua: &Lua, target: EntityHandle) -> i64 {
    let host = host(lua);
    let Some(ent) = host.entities.get(target) else { return class::NONE };
    if ent.player.is_some() {
        class::PLAYER
    } else if let Some(npc) = &ent.npc {
        npc.class
    } else if ent.kind == Kind::Npc {
        classify(&ent)
    } else {
        class::NONE
    }
}

/// Whether a target is there and alive: a living player, an NPC that has not died.
pub(crate) fn living(lua: &Lua, target: EntityHandle) -> bool {
    let host = host(lua);
    let Some(ent) = host.entities.get(target) else { return false };
    if ent.removed {
        return false;
    }
    match &ent.player {
        Some(p) => p.alive,
        None => !ent.npc.as_ref().is_some_and(|n| n.dead),
    }
}

/// Whether an NPC may take a target as its enemy: not one with `FL_NOTARGET`, nor any player
/// while `ai_ignoreplayers` is on - seen or shot by, alike.
pub(crate) fn targetable(lua: &Lua, target: EntityHandle) -> bool {
    let Some((player, flags)) = host(lua).entities.get(target).map(|e| (e.player.is_some(), e.flags)) else { return false };
    flags & FL_NOTARGET == 0 && !(player && number(lua, "ai_ignoreplayers").is_some_and(|n| n != 0.0))
}

/// How an NPC feels about a target, and how much: what it was told of that entity, else of its
/// class or name, else what its side feels.
pub(crate) fn disposition(lua: &Lua, handle: EntityHandle, target: EntityHandle) -> (i64, i64) {
    let (class, name) = host(lua).entities.get(target).map(|e| (e.class.clone(), e.name.clone())).unwrap_or_default();
    let told = with_npc(lua, handle, |_, npc| {
        let by_entity = npc.relationships.iter().find(|r| r.target == Target::Entity(target));
        let by_name = || {
            npc.relationships.iter().rev().find(|r| match &r.target {
                Target::Named(n) => n.eq_ignore_ascii_case(&class) || (!name.is_empty() && n.eq_ignore_ascii_case(&name)),
                Target::Entity(_) => false,
            })
        };
        by_entity.or_else(by_name).map(|r| (r.disposition, r.priority))
    });
    match told {
        Some(Some(found)) => found,
        Some(None) => (default_disposition(class_of_entity(lua, handle), class_of_entity(lua, target)), 0),
        None => (D_NU, 0),
    }
}

/// Keeps a relationship, in place of one with the same target.
fn relate(lua: &Lua, handle: EntityHandle, target: Target, disposition: i64, priority: i64) {
    with_npc(lua, handle, |_, npc| {
        npc.relationships.retain(|r| r.target != target);
        npc.relationships.push(Relationship { target, disposition, priority });
    });
}

/// `D_HT` and the like, by name or number.
fn parse_disposition(text: &str) -> Option<i64> {
    match text.to_ascii_uppercase().as_str() {
        "D_ER" => Some(D_ER),
        "D_HT" => Some(D_HT),
        "D_FR" => Some(D_FR),
        "D_LI" => Some(D_LI),
        "D_NU" => Some(D_NU),
        other => other.parse().ok(),
    }
}

/// What an NPC of a citizen's class wears: `citizentype` says the clothes (refugees, rebels,
/// medics), the spawn flags whether a man or a woman, chosen at random.
fn citizen_model(lua: &Lua, ent: &Ent) -> String {
    let kind = ent.keyvalue("citizentype").map_or(0, |v| crate::convar::atof(v) as i64);
    let group = match kind {
        2 => "group02",
        3 if ent.spawn_flags & SF_CITIZEN_MEDIC != 0 => "group03m",
        3 => "group03",
        _ => "group01",
    };
    const MEN: [&str; 9] = ["male_01", "male_02", "male_03", "male_04", "male_05", "male_06", "male_07", "male_08", "male_09"];
    const WOMEN: [&str; 6] = ["female_01", "female_02", "female_03", "female_04", "female_06", "female_07"];
    let heads: Vec<&str> = if ent.spawn_flags & SF_CITIZEN_RANDOM_HEAD_FEMALE != 0 {
        WOMEN.to_vec()
    } else if ent.spawn_flags & SF_CITIZEN_RANDOM_HEAD_MALE != 0 {
        MEN.to_vec()
    } else {
        MEN.iter().chain(WOMEN.iter()).copied().collect()
    };
    let head = heads[crate::anim::random(lua, heads.len() as i64) as usize];
    format!("models/humans/{group}/{head}.mdl")
}

/// An engine NPC has spawned ([`crate::ents::spawn`]): it takes its class's model, health,
/// hull and way of moving, stands on the floor, idles, and takes its weapon. One of a map
/// that is a template (`SF_NPC_TEMPLATE`) is not there to live but for an `npc_template_maker`
/// to make others by, and goes as the map loads.
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    if handle.kind != Kind::Npc {
        return;
    }
    let template = host(lua).entities.get(handle).is_some_and(|e| e.spawn_flags & SF_NPC_TEMPLATE != 0 && e.map_creation_id.is_some());
    if template {
        crate::ents::remove(lua, handle);
        return;
    }
    let Some(model) = host(lua).entities.get(handle).filter(|e| !e.scripted).map(|e| {
        let info = class_of(&e.class);
        match (e.model.is_empty(), info.name) {
            (false, _) => String::new(),
            (true, "npc_citizen") => citizen_model(lua, &e),
            (true, _) => info.model.to_string(),
        }
    }) else {
        return;
    };
    if !model.is_empty() {
        entity::set_model(lua, handle, &model);
    }
    let class = host(lua).entities.get(handle).map(|e| e.class.clone()).unwrap_or_default();
    let info = class_of(&class);
    let health = match info.health {
        ("", default) => default,
        (convar, default) => number(lua, convar).map_or(default, |n| n as i64),
    };
    let point = with_npc(lua, handle, |ent, npc| {
        if ent.health <= 0 {
            ent.health = health.max(1);
        }
        if ent.max_health <= 0 {
            ent.max_health = ent.health;
        }
        if let Some((mins, maxs)) = info.hull.and_then(hull_box) {
            ent.mins = mins;
            ent.maxs = maxs;
        }
        // one with no box (`npc_enemyfinder`, `npc_bullseye`) is a point nothing runs into
        let point = ent.mins == Vector::default() && ent.maxs == Vector::default();
        ent.solid = if point { SOLID_NONE } else { SOLID_BBOX };
        ent.move_type = match info.place {
            Place::Ground => MOVETYPE_STEP,
            Place::Air => MOVETYPE_FLY,
            Place::Fixed => MOVETYPE_NONE,
        };
        ent.collision_group = crate::trace::group::NPC;
        ent.flags |= FL_NPC;
        if !npc.class_set {
            npc.class = classify(ent);
        }
        npc.capabilities |= info.caps;
        npc.schedule = SCHED_IDLE_STAND;
        if let Some(squad) = ent.keyvalue("squadname") {
            npc.squad = squad.to_string();
        }
        point
    });
    if info.place == Place::Ground && point == Some(false) {
        drop_to_floor(lua, handle);
    }
    play_activity(lua, handle, ACT_IDLE);
    let equipment = host(lua).entities.get(handle).and_then(|e| e.keyvalue("additionalequipment").map(str::to_string));
    if let Some(class) = equipment.filter(|c| !c.is_empty() && !c.eq_ignore_ascii_case("none")) {
        let _ = give(lua, handle, &class);
    }
}

/// Down onto what is below, up to 256 units, as `DropToFloor`.
fn drop_to_floor(lua: &Lua, handle: EntityHandle) {
    let Some((pos, _)) = host(lua).entities.transform(handle) else { return };
    let Some((mins, maxs)) = host(lua).entities.get(handle).map(|e| (e.mins, e.maxs)) else { return };
    let sweep = crate::trace::Sweep {
        mins,
        maxs,
        mask: gmod_bsp::mask::NPCSOLID,
        ..crate::trace::Sweep::line(pos, pos - Vector::new(0.0, 0.0, 256.0), crate::trace::Filter::Entities(vec![handle]))
    };
    let hit = crate::trace::trace(lua, &sweep);
    if !hit.all_solid && hit.fraction < 1.0 {
        entity::set_world_pos(lua, handle, hit.end);
    }
}

/// Plays a sequence of an activity (chosen by weight), from its start; the activity is kept
/// either way. Whether the model had one.
pub(crate) fn play_activity(lua: &Lua, handle: EntityHandle, act: i64) -> bool {
    let model = host(lua).entities.get(handle).map(|e| e.model.clone()).unwrap_or_default();
    host(lua).entities.with_mut(handle, |e| e.anim.activity = act);
    let Some(rig) = crate::anim::rig(lua, &model) else { return false };
    let sequence = crate::anim::select_weighted(lua, &rig, act);
    if sequence < 0 {
        return false;
    }
    crate::anim::play(lua, handle, sequence);
    true
}

/// `NPC:Give`: a new weapon of a class in the NPC's hand, in place of the one it held;
/// `NULL` when the class is not a weapon.
pub(crate) fn give(lua: &Lua, handle: EntityHandle, class: &str) -> mlua::Result<EntityHandle> {
    let Some(item) = crate::ents::create(lua, class, None)? else { return Ok(EntityHandle::NULL) };
    if item.kind != Kind::Weapon {
        crate::ents::remove(lua, item);
        return Ok(EntityHandle::NULL);
    }
    // an engine weapon is seen as its world model
    let unseen = host(lua).entities.get(item).is_some_and(|e| e.model.is_empty());
    if unseen && let Some(data) = crate::weapon::data(lua, item).filter(|d| !d.world_model.is_empty()) {
        entity::set_model(lua, item, &data.world_model);
    }
    let (pos, _) = host(lua).entities.transform(handle).unwrap_or_default();
    entity::set_world_pos(lua, item, pos);
    crate::ents::spawn(lua, item);
    if !host(lua).entities.is_valid(item) {
        return Ok(EntityHandle::NULL);
    }
    equip(lua, handle, item);
    Ok(item)
}

/// A weapon goes into an NPC's hand: it follows the NPC's bones, and the one held before goes.
fn equip(lua: &Lua, handle: EntityHandle, item: EntityHandle) {
    let Some(old) = with_npc(lua, handle, |_, npc| std::mem::replace(&mut npc.weapon, item)) else { return };
    if old != item && host(lua).entities.get(old).is_some_and(|e| e.owner == handle) {
        crate::ents::remove(lua, old);
    }
    entity::set_parent(lua, item, handle);
    host(lua).entities.with_mut(item, |ent| {
        ent.owner = handle;
        ent.local_pos = Vector::default();
        ent.local_angles = Angle::default();
        ent.solid = 0;
        ent.move_type = MOVETYPE_NONE;
        ent.velocity = Vector::default();
        ent.effects = (ent.effects | EF_BONEMERGE) & !EF_NODRAW;
    });
    crate::weapon::call(lua, item, "Equip", MultiValue::from_iter([value(lua, handle)]));
    crate::weapon::call(lua, item, "OwnerChanged", MultiValue::new());
}

/// The weapon in an NPC's hand, if it holds one.
pub(crate) fn weapon_of(lua: &Lua, handle: EntityHandle) -> Option<EntityHandle> {
    with_npc(lua, handle, |_, npc| npc.weapon).filter(|&w| host(lua).entities.is_valid(w))
}

/// The weapon leaves the NPC's hand for the floor where it stands.
fn drop_weapon(lua: &Lua, handle: EntityHandle) -> EntityHandle {
    let Some(item) = with_npc(lua, handle, |_, npc| std::mem::replace(&mut npc.weapon, EntityHandle::NULL)) else {
        return EntityHandle::NULL;
    };
    if !host(lua).entities.is_valid(item) {
        return EntityHandle::NULL;
    }
    entity::set_parent(lua, item, EntityHandle::NULL);
    host(lua).entities.with_mut(item, |ent| {
        ent.owner = EntityHandle::NULL;
        ent.solid = SOLID_BBOX;
        ent.effects &= !(EF_BONEMERGE | EF_NODRAW);
    });
    crate::weapon::call(lua, item, "OnDrop", MultiValue::new());
    crate::weapon::call(lua, item, "OwnerChanged", MultiValue::new());
    item
}

/// Makes `enemy` the NPC's enemy, remembered where it is now; `OnFoundEnemy` fires for a new one.
pub(crate) fn set_enemy(lua: &Lua, handle: EntityHandle, enemy: EntityHandle) {
    let now = host(lua).clock.cur_time.get();
    let position = host(lua).entities.transform(enemy).map(|(p, _)| p);
    let found = with_npc(lua, handle, |_, npc| {
        let new = npc.enemy != enemy;
        npc.enemy = enemy;
        if let Some(position) = position {
            remember(npc, enemy, position, now);
        }
        new && enemy.index.is_some()
    });
    if found == Some(true) {
        crate::outputs::fire(lua, handle, "OnFoundEnemy", enemy, handle, None);
    }
}

fn remember(npc: &mut Npc, enemy: EntityHandle, position: Vector, now: f64) {
    match npc.memory.iter_mut().find(|m| m.enemy == enemy) {
        Some(m) => {
            m.position = position;
            m.last_seen = now;
        }
        None => npc.memory.push(Memory { enemy, position, first_seen: now, last_seen: now }),
    }
}

/// Damage to an engine NPC, after the `EntityTakeDamage` hook let it through
/// ([`crate::damage::take`]). `ScaleNPCDamage` is not for it: GMod asks that hook where a
/// bullet's trace hits a hit group, before the damage is taken ([`crate::bullets`]), and never
/// for damage given directly (`TakeDamageInfo`).
pub(crate) fn hurt(lua: &Lua, handle: EntityHandle, info: DamageInfo) {
    if handle.kind != Kind::Npc {
        return;
    }
    let alive = host(lua).entities.get(handle).is_some_and(|e| !e.removed && e.spawned && !e.npc.as_ref().is_some_and(|n| n.dead));
    if !alive {
        return;
    }
    let attacker = info.attacker.filter(|&a| host(lua).entities.is_valid(a)).unwrap_or(EntityHandle::NULL);
    let Some((health, half)) = with_npc(lua, handle, |ent, npc| {
        ent.health = (ent.health as f32 - info.damage) as i64;
        let half = !npc.half_health && ent.health <= ent.max_health / 2;
        npc.half_health |= half;
        (ent.health, half)
    }) else {
        return;
    };
    crate::outputs::fire(lua, handle, "OnDamaged", attacker, handle, None);
    if host(lua).entities.get(attacker).is_some_and(|e| e.player.is_some()) {
        crate::outputs::fire(lua, handle, "OnDamagedByPlayer", attacker, handle, None);
    }
    if half {
        crate::outputs::fire(lua, handle, "OnHalfHealth", attacker, handle, None);
    }
    // it turns on whoever hurt it, if it hates or fears them, may target them and has no
    // other enemy
    let enemy = with_npc(lua, handle, |_, npc| npc.enemy).unwrap_or(EntityHandle::NULL);
    if !living(lua, enemy) && attacker != handle && attacker.index.is_some_and(|i| i != 0) && targetable(lua, attacker) {
        let (feeling, _) = disposition(lua, handle, attacker);
        if feeling == D_HT || feeling == D_FR {
            set_enemy(lua, handle, attacker);
        }
    }
    if health <= 0 {
        die(lua, handle, &info);
    }
}

/// An NPC dies: `OnNPCKilled`, `OnDeath`, its weapon falls, its corpse is left, and it goes.
fn die(lua: &Lua, handle: EntityHandle, info: &DamageInfo) {
    let first = with_npc(lua, handle, |ent, npc| {
        let first = !npc.dead;
        npc.dead = true;
        npc.state = NPC_STATE_DEAD;
        npc.enemy = EntityHandle::NULL;
        ent.health = ent.health.min(0);
        first
    });
    if first != Some(true) {
        return;
    }
    let valid = |h: Option<EntityHandle>| h.filter(|&h| host(lua).entities.is_valid(h)).unwrap_or(EntityHandle::NULL);
    let (attacker, inflictor) = (valid(info.attacker), valid(info.inflictor));
    hook(lua, "OnNPCKilled", [value(lua, handle), value(lua, attacker), value(lua, inflictor)]);
    crate::outputs::fire(lua, handle, "OnDeath", attacker, handle, None);
    let flags = host(lua).entities.get(handle).map_or(0, |e| e.spawn_flags);
    let item = drop_weapon(lua, handle);
    if flags & SF_NPC_NO_WEAPON_DROP != 0 && item.index.is_some() {
        crate::ents::remove(lua, item);
    }
    if flags & SF_NPC_DROP_HEALTHKIT != 0 {
        let _ = drop_item(lua, handle, "item_healthvial");
    }
    let _ = corpse(lua, handle, info, flags & SF_NPC_FADE_CORPSE != 0);
    crate::ents::remove(lua, handle);
}

/// An item where the NPC stands, a little up.
fn drop_item(lua: &Lua, handle: EntityHandle, class: &str) -> mlua::Result<()> {
    let Some(item) = crate::ents::create(lua, class, None)? else { return Ok(()) };
    let (pos, _) = host(lua).entities.transform(handle).unwrap_or_default();
    entity::set_world_pos(lua, item, pos + Vector::new(0.0, 0.0, 16.0));
    crate::ents::spawn(lua, item);
    Ok(())
}

/// The ragdoll of a dead NPC, posed as it was drawn and pushed by the blow that killed it: the
/// client's stand-in (`ai_serverragdolls 0`) or a `prop_ragdoll` of the server's.
fn corpse(lua: &Lua, handle: EntityHandle, info: &DamageInfo, fades: bool) -> mlua::Result<()> {
    let Some(ent) = host(lua).entities.get(handle).map(|e| Ent::clone(&e)) else { return Ok(()) };
    if crate::anim::rig(lua, &ent.model).is_none() || crate::physics::solids(lua, &ent.model) == 0 {
        return Ok(());
    }
    let server = number(lua, "ai_serverragdolls").is_some_and(|n| n != 0.0);
    let made = if server {
        crate::ents::create(lua, "prop_ragdoll", None)?
    } else {
        crate::ents::create_engine(lua, CLIENT_RAGDOLL, Kind::Entity)?
    };
    let Some(body) = made else { return Ok(()) };
    let (pos, angles) = host(lua).entities.transform(handle).unwrap_or_default();
    entity::set_model(lua, body, &ent.model);
    entity::set_world_pos(lua, body, pos);
    entity::set_world_angles(lua, body, angles);
    host(lua).entities.with_mut(body, |e| {
        e.skin = ent.skin;
        e.color = ent.color;
        e.material = ent.material.clone();
        e.body_groups = ent.body_groups.clone();
        e.bone_manipulations = ent.bone_manipulations.clone();
        e.model_scale = ent.model_scale;
        e.anim = ent.anim.clone();
    });
    let made = if server {
        crate::ents::spawn(lua, body);
        crate::ragdoll::is_ragdoll(lua, body)
    } else {
        crate::ragdoll::init(lua, body, ent.velocity)
    };
    if !made {
        crate::ents::remove(lua, body);
        return Ok(());
    }
    let parts = crate::ragdoll::bodies(lua, body).unwrap_or_default();
    {
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        // the client's corpse is pushed by nothing that moves on the server
        if !server {
            for &id in &parts {
                physics.set_aloof(id, true);
            }
        }
    }
    push(lua, body, info);
    if server {
        hook(lua, "CreateEntityRagdoll", [value(lua, handle), value(lua, body)]);
    }
    // the server's ragdolls stay till they fade, however many; the client's go oldest first
    if fades || !server {
        let fades_at = fades.then(|| host(lua).clock.cur_time.get() + CORPSE_TIME);
        host(lua).npcs.corpses.borrow_mut().push(Corpse { body, fades_at, client: !server });
    }
    if !server {
        retire_corpses(lua);
    }
    Ok(())
}

/// The oldest of the client's corpses go while there are more than [`CORPSE_CAP`].
fn retire_corpses(lua: &Lua) {
    let retired: Vec<EntityHandle> = {
        let host = host(lua);
        let mut list = host.npcs.corpses.borrow_mut();
        list.retain(|c| host.entities.get(c.body).is_some_and(|e| !e.removed));
        let mut over = list.iter().filter(|c| c.client).count().saturating_sub(CORPSE_CAP);
        let mut retired = Vec::new();
        list.retain(|c| {
            let keep = over == 0 || !c.client;
            if !keep {
                over -= 1;
                retired.push(c.body);
            }
            keep
        });
        retired
    };
    for body in retired {
        crate::ents::remove(lua, body);
    }
}

/// The force of the killing blow pushes the part nearest where it hit (the first part when it
/// says nowhere).
fn push(lua: &Lua, body: EntityHandle, info: &DamageInfo) {
    if info.force == Vector::default() {
        return;
    }
    let count = crate::ragdoll::count(lua, body).unwrap_or(0);
    let at = info.position;
    let nearest = if at == Vector::default() {
        0
    } else {
        (0..count)
            .filter_map(|part| crate::ragdoll::part_pose(lua, body, part).map(|(p, _)| (part, (p - at).length())))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(0, |(part, _)| part)
    };
    let Some(id) = crate::physics::part_body(lua, body, nearest) else { return };
    let host = host(lua);
    let mut physics = host.physics.physics.borrow_mut();
    let force = crate::physics::array(info.force);
    if at == Vector::default() {
        physics.apply_impulse(id, force);
    } else {
        physics.apply_impulse_at(id, force, crate::physics::array(at));
    }
}

/// An entity is going ([`crate::ents::remove`]): the weapon in an NPC's hand goes with it. Only
/// on the server: a client removes what left the server's snapshot while it still has the old
/// state, where the NPC holds a weapon the server dropped and still sends.
pub(crate) fn removed(lua: &Lua, handle: EntityHandle) {
    if handle.kind != Kind::Npc || host(lua).realm != crate::Realm::Server {
        return;
    }
    let item = host(lua).entities.get(handle).and_then(|e| e.npc.as_ref().map(|n| n.weapon)).unwrap_or(EntityHandle::NULL);
    if host(lua).entities.get(item).is_some_and(|e| e.owner == handle) {
        crate::ents::remove(lua, item);
    }
}

/// One tick: fading corpses fade and go; NPCs look for enemies and turn to face them.
pub(crate) fn tick(lua: &Lua) {
    let now = host(lua).clock.cur_time.get();
    let corpses: Vec<(EntityHandle, f64)> = {
        let host = host(lua);
        let mut list = host.npcs.corpses.borrow_mut();
        list.retain(|c| host.entities.is_valid(c.body));
        list.iter().filter_map(|c| c.fades_at.map(|at| (c.body, at))).collect()
    };
    for (corpse, at) in corpses {
        if now >= at {
            crate::ents::remove(lua, corpse);
        } else if at - now < CORPSE_FADE {
            let alpha = (255.0 * (at - now) / CORPSE_FADE).clamp(0.0, 255.0) as u8;
            host(lua).entities.with_mut(corpse, |e| {
                e.render_mode = RENDERMODE_TRANSALPHA;
                e.color[3] = alpha;
            });
        }
    }
    if number(lua, "ai_disabled").is_some_and(|n| n != 0.0) {
        return;
    }
    let thinking: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|&h| h.kind == Kind::Npc)
            .filter(|&h| entities.get(h).is_some_and(|e| e.spawned && !e.removed && !e.scripted && e.npc.as_ref().is_some_and(|n| !n.dead)))
            .collect()
    };
    for handle in thinking {
        think(lua, handle, now);
    }
}

fn think(lua: &Lua, handle: EntityHandle, now: f64) {
    let Some((enemy, look)) = with_npc(lua, handle, |_, npc| (npc.enemy, npc.next_look <= now)) else { return };
    // one dead or gone, or that it may no longer target, it lets go of, still alert
    if enemy.index.is_some() && !(living(lua, enemy) && targetable(lua, enemy)) {
        with_npc(lua, handle, |_, npc| {
            npc.enemy = EntityHandle::NULL;
            if npc.state == NPC_STATE_COMBAT {
                npc.state = NPC_STATE_ALERT;
            }
        });
    }
    let enemy = with_npc(lua, handle, |_, npc| npc.enemy).unwrap_or(EntityHandle::NULL);
    if enemy.index.is_none() && look {
        with_npc(lua, handle, |_, npc| npc.next_look = now + LOOK_INTERVAL);
        if let Some(found) = crate::ai::nearest_enemy(lua, handle) {
            set_enemy(lua, handle, found);
            with_npc(lua, handle, |_, npc| npc.state = NPC_STATE_COMBAT);
        }
    } else if look && enemy.index.is_some() {
        // what it sees of its enemy it remembers
        with_npc(lua, handle, |_, npc| npc.next_look = now + LOOK_INTERVAL);
        if crate::ai::sees(lua, handle, enemy) {
            let position = host(lua).entities.transform(enemy).map(|(p, _)| p);
            with_npc(lua, handle, |_, npc| {
                if let Some(position) = position {
                    remember(npc, enemy, position, now);
                }
            });
        }
    }
    crate::ai::think(lua, handle, timer::TICK_INTERVAL);
}

/// Where an NPC's eyes are: near the top of its box.
pub(crate) fn eyes(lua: &Lua, handle: EntityHandle) -> Option<Vector> {
    let (pos, _) = host(lua).entities.transform(handle)?;
    let (mins, maxs) = host(lua).entities.get(handle).map(|e| (e.mins, e.maxs))?;
    Some(pos + Vector::new(0.0, 0.0, mins.z + (maxs.z - mins.z) * 0.9))
}

/// Turns an NPC about its yaw towards a point, as far as it turns in `seconds`.
pub(crate) fn turn_towards(lua: &Lua, handle: EntityHandle, target: Vector, seconds: f32) {
    let Some((pos, angles)) = host(lua).entities.transform(handle) else { return };
    let (dx, dy) = (target.x - pos.x, target.y - pos.y);
    if dx.abs() < 0.01 && dy.abs() < 0.01 {
        return;
    }
    let want = dy.atan2(dx).to_degrees();
    let off = (want - angles.y + 540.0).rem_euclid(360.0) - 180.0;
    if off.abs() < 0.01 {
        return;
    }
    let step = TURN_SPEED * seconds;
    entity::set_world_angles(lua, handle, Angle::new(angles.p, angles.y + off.clamp(-step, step), angles.r));
}

/// The living NPCs of a squad.
fn squad(lua: &Lua, name: &str) -> Vec<EntityHandle> {
    let entities = &host(lua).entities;
    entities
        .handles()
        .into_iter()
        .filter(|&h| {
            entities.get(h).is_some_and(|e| {
                !e.removed && e.npc.as_ref().is_some_and(|n| !n.dead && !name.is_empty() && n.squad.eq_ignore_ascii_case(name))
            })
        })
        .collect()
}

// --- Lua ---

/// The NPC in argument 1.
fn this(a: &Args) -> mlua::Result<EntityHandle> {
    let handle = entity::this(a, 1)?;
    if handle.kind == Kind::Npc { Ok(handle) } else { Err(a.bad(1, "NPC")) }
}

fn read<R>(a: &Args, f: impl FnOnce(&Ent, &Npc) -> R) -> mlua::Result<R> {
    let handle = this(a)?;
    with_npc(a.lua, handle, |ent, npc| f(ent, npc)).ok_or_else(|| a.bad(1, "NPC"))
}

fn write(a: &Args, f: impl FnOnce(&mut Ent, &mut Npc)) -> mlua::Result<Value> {
    let handle = this(a)?;
    with_npc(a.lua, handle, f);
    Ok(Value::Nil)
}

fn integer(n: i64) -> mlua::Result<Value> {
    Ok(Value::Integer(n))
}

fn entity_arg(a: &Args, position: usize) -> EntityHandle {
    entity::handle_of(a.get(position)).filter(|&h| host(a.lua).entities.is_valid(h)).unwrap_or(EntityHandle::NULL)
}

/// An entity while it is there, else `NULL`.
fn push_valid(lua: &Lua, handle: EntityHandle) -> mlua::Result<Value> {
    entity::push(lua, if host(lua).entities.is_valid(handle) { handle } else { EntityHandle::NULL })
}

fn list(lua: &Lua, handles: impl IntoIterator<Item = EntityHandle>) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    for handle in handles {
        t.raw_push(entity::push(lua, handle)?)?;
    }
    Ok(Value::Table(t))
}

fn nothing(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    Ok(Value::Nil)
}

fn no(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    Ok(Value::Boolean(false))
}

fn zero(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    Ok(Value::Integer(0))
}

fn own_pos(a: &Args) -> mlua::Result<Value> {
    let handle = this(a)?;
    entity::vector(a.lua, host(a.lua).entities.transform(handle).map(|(p, _)| p).unwrap_or_default())
}

/// The memory of the enemy in argument 2, or of the NPC's enemy.
fn memory_of<R>(a: &Args, f: impl FnOnce(Option<&Memory>) -> R) -> mlua::Result<R> {
    let asked = entity::handle_of(a.get(2));
    read(a, |_, npc| {
        let enemy = asked.unwrap_or(npc.enemy);
        f(npc.memory.iter().find(|m| m.enemy == enemy))
    })
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    if let Some(ai) = lua.globals().get::<Option<Table>>("ai")? {
        install_ai(lua, &ai)?;
    }
    let meta = meta::meta_table(lua, "NPC", type_id::ENTITY)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        meta.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    install_weapons(&def)?;
    install_enemies(&def)?;
    install_relationships(&def)?;
    install_schedules(&def)?;
    install_activities(&def)?;
    install_movement(&def)?;
    install_rest(&def)?;
    meta.raw_set(
        "Disposition",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "Disposition");
            let handle = this(&a)?;
            let target = entity::this(&a, 2)?;
            let (feeling, priority) = disposition(lua, handle, target);
            Ok(MultiValue::from_iter([Value::Integer(feeling), Value::Integer(priority)]))
        })?,
    )?;
    Ok(())
}

/// Squads, in the `ai` library.
fn install_ai(lua: &Lua, ai: &Table) -> mlua::Result<()> {
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        ai.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("GetSquadMemberCount", |a| integer(squad(a.lua, &a.string(1)?).len() as i64))?;
    def("GetSquadMembers", |a| list(a.lua, squad(a.lua, &a.string(1)?)))?;
    def("GetSquadLeader", |a| entity::push(a.lua, squad(a.lua, &a.string(1)?).first().copied().unwrap_or(EntityHandle::NULL)))
}

fn install_weapons(def: &Def) -> mlua::Result<()> {
    def("Give", |a| {
        let handle = this(a)?;
        let class = a.string(2)?;
        let item = give(a.lua, handle, &class)?;
        entity::push(a.lua, item)
    })?;
    def("GetActiveWeapon", |a| {
        let item = read(a, |_, npc| npc.weapon)?;
        push_valid(a.lua, item)
    })?;
    def("GetWeapons", |a| {
        let item = read(a, |_, npc| npc.weapon)?;
        list(a.lua, Some(item).filter(|&h| host(a.lua).entities.is_valid(h)))
    })?;
    def("GetWeapon", |a| {
        let item = read(a, |_, npc| npc.weapon)?;
        let class = a.string(2)?;
        let held = host(a.lua).entities.get(item).is_some_and(|e| e.class.eq_ignore_ascii_case(&class));
        entity::push(a.lua, if held { item } else { EntityHandle::NULL })
    })?;
    def("PickupWeapon", |a| {
        let handle = this(a)?;
        let item = entity::this(a, 2)?;
        if item.kind != Kind::Weapon || host(a.lua).entities.get(item).is_some_and(|e| e.owner.index.is_some()) {
            return Ok(Value::Boolean(false));
        }
        equip(a.lua, handle, item);
        Ok(Value::Boolean(true))
    })?;
    def("DropWeapon", |a| {
        let handle = this(a)?;
        let held = read(a, |_, npc| npc.weapon)?;
        let asked = entity_arg(a, 2);
        if asked.index.is_none() || asked == held {
            let item = drop_weapon(a.lua, handle);
            if let Some(velocity) = a.get(4).and_then(|_| a.userdata::<Vector>(4, "Vector").ok()) {
                host(a.lua).entities.with_mut(item, |e| e.velocity = velocity);
            }
        }
        Ok(Value::Nil)
    })?;
    def("SelectWeapon", nothing)?;
    def("GetCurrentWeaponProficiency", |a| integer(read(a, |_, npc| npc.proficiency)?))?;
    def("SetCurrentWeaponProficiency", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.proficiency = n)
    })?;
    def("GetShootPos", |a| {
        let handle = this(a)?;
        entity::vector(a.lua, eyes(a.lua, handle).unwrap_or_default())
    })?;
    def("GetAimVector", |a| {
        let handle = this(a)?;
        let (_, angles) = host(a.lua).entities.transform(handle).unwrap_or_default();
        let enemy = read(a, |_, npc| npc.enemy)?;
        let aim = match (eyes(a.lua, handle), host(a.lua).entities.transform(enemy)) {
            (Some(from), Some((to, _))) if living(a.lua, enemy) => (to - from).normalized(),
            _ => angles.vectors().0,
        };
        entity::vector(a.lua, aim)
    })
}

fn install_enemies(def: &Def) -> mlua::Result<()> {
    def("SetEnemy", |a| {
        let handle = this(a)?;
        let enemy = entity_arg(a, 2);
        set_enemy(a.lua, handle, enemy);
        Ok(Value::Nil)
    })?;
    def("GetEnemy", |a| {
        let enemy = read(a, |_, npc| npc.enemy)?;
        push_valid(a.lua, enemy)
    })?;
    def("UpdateEnemyMemory", |a| {
        let enemy = entity::this(a, 2)?;
        let position = a.userdata::<Vector>(3, "Vector")?;
        let now = host(a.lua).clock.cur_time.get();
        write(a, |_, npc| remember(npc, enemy, position, now))
    })?;
    def("HasEnemyMemory", |a| Ok(Value::Boolean(memory_of(a, |m| m.is_some())?)))?;
    def("ClearEnemyMemory", |a| {
        let asked = entity::handle_of(a.get(2));
        write(a, |_, npc| {
            match asked {
                Some(enemy) => npc.memory.retain(|m| m.enemy != enemy),
                None => npc.memory.clear(),
            }
            if asked.is_none_or(|e| e == npc.enemy) {
                npc.enemy = EntityHandle::NULL;
            }
        })
    })?;
    def("GetEnemyLastKnownPos", |a| {
        let pos = memory_of(a, |m| m.map(|m| m.position))?;
        match pos {
            Some(pos) => entity::vector(a.lua, pos),
            None => own_pos(a),
        }
    })?;
    def("GetEnemyLastSeenPos", |a| {
        let pos = memory_of(a, |m| m.map(|m| m.position))?;
        match pos {
            Some(pos) => entity::vector(a.lua, pos),
            None => own_pos(a),
        }
    })?;
    def("GetEnemyFirstTimeSeen", |a| Ok(Value::Number(memory_of(a, |m| m.map_or(0.0, |m| m.first_seen))?)))?;
    def("GetEnemyLastTimeSeen", |a| Ok(Value::Number(memory_of(a, |m| m.map_or(0.0, |m| m.last_seen))?)))?;
    def("GetTimeEnemyLastReacquired", |a| Ok(Value::Number(memory_of(a, |m| m.map_or(0.0, |m| m.last_seen))?)))?;
    def("GetKnownEnemies", |a| {
        let known = read(a, |_, npc| npc.memory.iter().map(|m| m.enemy).collect::<Vec<_>>())?;
        let lua = a.lua;
        list(lua, known.into_iter().filter(|&h| host(lua).entities.is_valid(h)))
    })?;
    def("GetKnownEnemyCount", |a| {
        let known = read(a, |_, npc| npc.memory.iter().map(|m| m.enemy).collect::<Vec<_>>())?;
        integer(known.into_iter().filter(|&h| host(a.lua).entities.is_valid(h)).count() as i64)
    })?;
    def("IsEnemy", |a| {
        let handle = this(a)?;
        let target = entity::this(a, 2)?;
        Ok(Value::Boolean(disposition(a.lua, handle, target).0 == D_HT))
    })?;
    def("MarkEnemyAsEluded", nothing)?;
    def("HasEnemyEluded", no)?;
    def("IgnoreEnemyUntil", nothing)?;
    def("SetTarget", |a| {
        let target = entity_arg(a, 2);
        write(a, |_, npc| npc.target = target)
    })?;
    def("GetTarget", |a| {
        let target = read(a, |_, npc| npc.target)?;
        push_valid(a.lua, target)
    })?;
    def("SetMaxLookDistance", |a| {
        let n = a.number(2)? as f32;
        write(a, |_, npc| npc.look_distance = n)
    })?;
    def("GetMaxLookDistance", |a| Ok(Value::Number(f64::from(read(a, |_, npc| npc.look_distance)?))))?;
    def("IsInViewCone", |a| {
        let handle = this(a)?;
        let point = match a.get(2) {
            Some(Value::UserData(ud)) if ud.borrow::<Vector>().is_ok() => *ud.borrow::<Vector>()?,
            _ => host(a.lua).entities.transform(entity::this(a, 2)?).map(|(p, _)| p).unwrap_or_default(),
        };
        let (pos, angles) = host(a.lua).entities.transform(handle).unwrap_or_default();
        let to = point - pos;
        let flat = Vector::new(to.x, to.y, 0.0).normalized();
        let forward = angles.vectors().0;
        // a cone of 90 degrees either side, as the engine's default
        Ok(Value::Boolean(flat.dot(Vector::new(forward.x, forward.y, 0.0).normalized()) >= 0.0))
    })
}

fn install_relationships(def: &Def) -> mlua::Result<()> {
    // "npc_combine_s D_HT 99": a class or a name, how it feels, how much; several in a row
    def("AddRelationship", |a| {
        let handle = this(a)?;
        let text = a.string(2)?;
        let words: Vec<&str> = text.split_whitespace().collect();
        for chunk in words.chunks(3) {
            let [target, feeling, rest @ ..] = chunk else { continue };
            let Some(feeling) = parse_disposition(feeling) else { continue };
            let priority = rest.first().map_or(0, |p| crate::convar::atof(p) as i64);
            relate(a.lua, handle, Target::Named(target.to_string()), feeling, priority);
        }
        Ok(Value::Nil)
    })?;
    def("AddEntityRelationship", |a| {
        let handle = this(a)?;
        let target = entity::this(a, 2)?;
        let feeling = a.number(3)? as i64;
        let priority = a.opt_number(4)?.unwrap_or(0.0) as i64;
        relate(a.lua, handle, Target::Entity(target), feeling, priority);
        Ok(Value::Nil)
    })?;
    def("Classify", |a| integer(read(a, |_, npc| npc.class)?))?;
    def("SetNPCClass", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| {
            npc.class = n;
            npc.class_set = true;
        })
    })?;
    def("SetSquad", |a| {
        let name = a.opt_string(2)?.unwrap_or_default();
        write(a, |_, npc| npc.squad = name)
    })?;
    def("GetSquad", |a| {
        let name = read(a, |_, npc| npc.squad.clone())?;
        if name.is_empty() { Ok(Value::Nil) } else { Ok(Value::String(a.lua.create_string(&name)?)) }
    })?;
    def("IsSquadLeader", |a| {
        let handle = this(a)?;
        let name = read(a, |_, npc| npc.squad.clone())?;
        Ok(Value::Boolean(squad(a.lua, &name).first() == Some(&handle)))
    })?;
    def("GetNearestSquadMember", |a| {
        let handle = this(a)?;
        let name = read(a, |_, npc| npc.squad.clone())?;
        let (pos, _) = host(a.lua).entities.transform(handle).unwrap_or_default();
        let nearest = squad(a.lua, &name)
            .into_iter()
            .filter(|&h| h != handle)
            .filter_map(|h| host(a.lua).entities.transform(h).map(|(p, _)| (h, (p - pos).length())))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .map_or(EntityHandle::NULL, |(h, _)| h);
        entity::push(a.lua, nearest)
    })
}

fn install_schedules(def: &Def) -> mlua::Result<()> {
    def("SetSchedule", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| {
            npc.schedule = n;
            crate::ai::schedule_set(npc);
        })
    })?;
    def("GetCurrentSchedule", |a| integer(read(a, |_, npc| npc.schedule)?))?;
    def("IsCurrentSchedule", |a| {
        let n = a.number(2)? as i64;
        Ok(Value::Boolean(read(a, |_, npc| npc.schedule == n)?))
    })?;
    def("ClearSchedule", |a| {
        let handle = this(a)?;
        crate::ai::stop(a.lua, handle);
        write(a, |_, npc| {
            npc.schedule = SCHED_NONE;
            crate::ai::schedule_set(npc);
        })
    })?;
    def("SetNPCState", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.state = n)
    })?;
    def("GetNPCState", |a| integer(read(a, |_, npc| npc.state)?))?;
    def("SetCondition", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| {
            if !npc.conditions.contains(&n) {
                npc.conditions.push(n);
            }
        })
    })?;
    def("ClearCondition", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.conditions.retain(|&c| c != n))
    })?;
    def("HasCondition", |a| {
        let n = a.number(2)? as i64;
        Ok(Value::Boolean(read(a, |_, npc| npc.conditions.contains(&n))?))
    })?;
    def("RemoveIgnoreConditions", nothing)?;
    def("CapabilitiesAdd", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.capabilities |= n)
    })?;
    def("CapabilitiesRemove", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.capabilities &= !n)
    })?;
    def("CapabilitiesClear", |a| write(a, |_, npc| npc.capabilities = 0))?;
    def("CapabilitiesGet", |a| integer(read(a, |_, npc| npc.capabilities)?))?;
    for name in ["TaskComplete", "TaskFail", "StartEngineTask", "RunEngineTask", "ExitScriptedSequence", "UseNoBehavior"] {
        def(name, nothing)?;
    }
    for name in
        ["UseActBusyBehavior", "UseAssaultBehavior", "UseFollowBehavior", "UseFuncTankBehavior", "UseLeadBehavior", "IsRunningBehavior"]
    {
        def(name, no)?;
    }
    Ok(())
}

fn install_activities(def: &Def) -> mlua::Result<()> {
    def("GetActivity", |a| {
        let handle = this(a)?;
        integer(host(a.lua).entities.get(handle).map_or(ACT_INVALID, |e| e.anim.activity))
    })?;
    def("SetActivity", |a| {
        let handle = this(a)?;
        play_activity(a.lua, handle, a.number(2)? as i64);
        Ok(Value::Nil)
    })?;
    def("GetIdealActivity", |a| {
        let handle = this(a)?;
        integer(host(a.lua).entities.get(handle).map_or(ACT_INVALID, |e| e.anim.activity))
    })?;
    def("SetIdealActivity", |a| {
        let handle = this(a)?;
        play_activity(a.lua, handle, a.number(2)? as i64);
        Ok(Value::Nil)
    })?;
    def("ResetIdealActivity", |a| {
        let handle = this(a)?;
        play_activity(a.lua, handle, a.number(2)? as i64);
        Ok(Value::Nil)
    })?;
    def("MaintainActivity", nothing)?;
    def("SetMovementActivity", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.movement_activity = n)
    })?;
    def("GetMovementActivity", |a| integer(read(a, |_, npc| npc.movement_activity)?))?;
    def("SetMovementSequence", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.movement_sequence = n)
    })?;
    def("GetMovementSequence", |a| integer(read(a, |_, npc| npc.movement_sequence)?))?;
    def("SetArrivalActivity", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.arrival_activity = n)
    })?;
    def("GetArrivalActivity", |a| integer(read(a, |_, npc| npc.arrival_activity)?))?;
    def("SetArrivalSequence", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.arrival_sequence = n)
    })?;
    def("GetArrivalSequence", |a| integer(read(a, |_, npc| npc.arrival_sequence)?))?;
    def("UpdateTurnActivity", nothing)
}

/// A goal Lua gives ([`crate::ai::set_goal`]): walked to, or run to when its movement activity
/// is the run.
fn lua_goal(a: &Args, goal: impl FnOnce(bool) -> crate::ai::Goal) -> mlua::Result<Value> {
    let handle = this(a)?;
    let run = read(a, |_, npc| npc.movement_activity == crate::ai::ACT_RUN)?;
    Ok(Value::Boolean(crate::ai::set_goal(a.lua, handle, goal(run))))
}

/// Moving: goals, the way to them and how it goes ([`crate::ai`]).
fn install_movement(def: &Def) -> mlua::Result<()> {
    const TOLERANCE: f32 = 12.0;
    def("NavSetGoal", |a| {
        let pos = a.userdata::<Vector>(2, "Vector")?;
        lua_goal(a, |run| crate::ai::Goal::point(pos, TOLERANCE, run))
    })?;
    def("NavSetGoalPos", |a| {
        let pos = a.userdata::<Vector>(2, "Vector")?;
        lua_goal(a, |run| crate::ai::Goal::point(pos, TOLERANCE, run))
    })?;
    def("NavSetGoalTarget", |a| {
        let target = entity::this(a, 2)?;
        let offset = a.get(3).and_then(|_| a.userdata::<Vector>(3, "Vector").ok()).unwrap_or_default();
        lua_goal(a, |run| crate::ai::Goal::after(target, offset, TOLERANCE * 4.0, run))
    })?;
    def("NavSetRandomGoal", |a| {
        let handle = this(a)?;
        let min = a.opt_number(2)?.unwrap_or(128.0) as f32;
        let (pos, _) = host(a.lua).entities.transform(handle).unwrap_or_default();
        let lua = a.lua;
        let roll = |below: usize| crate::anim::random(lua, below as i64) as usize;
        let hull = crate::nav::Hull {
            mins: host(lua).entities.get(handle).map(|e| e.mins).unwrap_or_default(),
            maxs: host(lua).entities.get(handle).map(|e| e.maxs).unwrap_or_default(),
            air: host(lua).entities.get(handle).is_some_and(|e| e.move_type == MOVETYPE_FLY),
        };
        let Some(spot) = crate::nav::random_spot(lua, pos, min, min.max(128.0) * 2.0, &hull, roll) else {
            return Ok(Value::Boolean(false));
        };
        lua_goal(a, |run| crate::ai::Goal::point(spot, TOLERANCE, run))
    })?;
    def("NavSetWanderGoal", |a| {
        let handle = this(a)?;
        let min = a.opt_number(2)?.unwrap_or(128.0) as f32;
        let max = a.opt_number(3)?.map_or(min.max(128.0) * 2.0, |m| m as f32);
        let (pos, _) = host(a.lua).entities.transform(handle).unwrap_or_default();
        let lua = a.lua;
        let roll = |below: usize| crate::anim::random(lua, below as i64) as usize;
        let hull = crate::nav::Hull {
            mins: host(lua).entities.get(handle).map(|e| e.mins).unwrap_or_default(),
            maxs: host(lua).entities.get(handle).map(|e| e.maxs).unwrap_or_default(),
            air: host(lua).entities.get(handle).is_some_and(|e| e.move_type == MOVETYPE_FLY),
        };
        let Some(spot) = crate::nav::random_spot(lua, pos, min, max.max(min), &hull, roll) else { return Ok(Value::Boolean(false)) };
        lua_goal(a, |run| crate::ai::Goal::point(spot, TOLERANCE, run))
    })?;
    def("IsMoving", |a| Ok(Value::Boolean(read(a, |_, npc| npc.mind.goal.is_some() && !npc.mind.route.is_empty())?)))?;
    def("IsGoalActive", |a| Ok(Value::Boolean(read(a, |_, npc| npc.mind.goal.is_some())?)))?;
    def("StopMoving", |a| {
        let handle = this(a)?;
        crate::ai::stop(a.lua, handle);
        Ok(Value::Nil)
    })?;
    def("ClearGoal", |a| {
        let handle = this(a)?;
        crate::ai::stop(a.lua, handle);
        Ok(Value::Nil)
    })?;
    def("HasObstacles", no)?;
    def("IsUnreachable", |a| {
        let handle = this(a)?;
        let target = entity_arg(a, 2);
        Ok(Value::Boolean(crate::ai::is_unreachable(a.lua, handle, target)))
    })?;
    def("RememberUnreachable", |a| {
        let handle = this(a)?;
        let target = entity::this(a, 2)?;
        let seconds = a.opt_number(3)?.unwrap_or(3.0);
        crate::ai::remember_unreachable(a.lua, handle, target, seconds);
        Ok(Value::Nil)
    })?;
    def("SetArrivalDistance", |a| {
        let n = a.number(2)? as f32;
        write(a, |_, npc| npc.mind.arrival_distance = Some(n))
    })?;
    for name in ["SetArrivalDirection", "SetArrivalSpeed", "SetMaxRouteRebuildTime", "SetMoveVelocity"] {
        def(name, nothing)?;
    }
    def("SetNavType", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.mind.nav_type = Some(n))
    })?;
    def("GetNavType", |a| {
        let handle = this(a)?;
        let set = read(a, |_, npc| npc.mind.nav_type)?;
        let by_movement = match host(a.lua).entities.get(handle).map_or(MOVETYPE_NONE, |e| e.move_type) {
            MOVETYPE_STEP => 0,
            MOVETYPE_FLY => 2,
            _ => -1,
        };
        integer(set.unwrap_or(by_movement))
    })?;
    // a citizen of the player's squad sent somewhere, and told what to go for
    def("MoveOrder", |a| {
        let pos = a.userdata::<Vector>(2, "Vector")?;
        write(a, |_, npc| {
            npc.last_position = pos;
            npc.schedule = crate::ai::sched::FORCED_GO_RUN;
            crate::ai::schedule_set(npc);
        })
    })?;
    def("TargetOrder", |a| {
        let target = entity_arg(a, 2);
        write(a, |_, npc| npc.target = target)
    })?;
    def("GetPathDistanceToGoal", |a| {
        let handle = this(a)?;
        Ok(Value::Number(f64::from(crate::ai::path_length(a.lua, handle))))
    })?;
    def("GetPathTimeToGoal", |a| {
        let handle = this(a)?;
        let speed = crate::ai::speed(a.lua, handle);
        let length = crate::ai::path_length(a.lua, handle);
        Ok(Value::Number(if speed > 0.0 { f64::from(length / speed) } else { 0.0 }))
    })?;
    def("GetIdealMoveSpeed", |a| {
        let handle = this(a)?;
        Ok(Value::Number(f64::from(crate::ai::speed(a.lua, handle))))
    })?;
    def("GetIdealMoveAcceleration", zero)?;
    def("GetBlockingEntity", |a| {
        let blocker = read(a, |_, npc| npc.mind.blocker)?;
        push_valid(a.lua, blocker)
    })?;
    def("GetCurrentWaypointPos", |a| match read(a, |_, npc| npc.mind.route.first().copied())? {
        Some(pos) => entity::vector(a.lua, pos),
        None => own_pos(a),
    })?;
    def("GetNextWaypointPos", |a| match read(a, |_, npc| npc.mind.route.get(1).or(npc.mind.route.first()).copied())? {
        Some(pos) => entity::vector(a.lua, pos),
        None => own_pos(a),
    })?;
    def("GetMoveVelocity", |a| entity::vector(a.lua, read(a, |_, npc| npc.mind.velocity)?))?;
    def("SetLastPosition", |a| {
        let pos = a.userdata::<Vector>(2, "Vector")?;
        write(a, |_, npc| npc.last_position = pos)
    })?;
    def("GetLastPosition", |a| entity::vector(a.lua, read(a, |_, npc| npc.last_position)?))?;
    def("SetIdealYawAndUpdate", |a| {
        let handle = this(a)?;
        let yaw = a.number(2)? as f32;
        let (_, angles) = host(a.lua).entities.transform(handle).unwrap_or_default();
        entity::set_world_angles(a.lua, handle, Angle::new(angles.p, yaw, angles.r));
        Ok(Value::Nil)
    })
}

fn install_rest(def: &Def) -> mlua::Result<()> {
    def("GetHullType", |a| integer(read(a, |_, npc| npc.hull)?))?;
    def("SetHullType", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, npc| npc.hull = n)
    })?;
    def("SetHullSizeNormal", |a| {
        write(a, |ent, npc| {
            if let Some((mins, maxs)) = hull_box(npc.hull) {
                ent.mins = mins;
                ent.maxs = maxs;
            }
        })
    })?;
    // what is said and shown is quiet
    for name in ["AlertSound", "IdleSound", "FoundEnemySound", "LostEnemySound", "FearSound", "SentenceStop", "ClearExpression"] {
        def(name, nothing)?;
    }
    def("PlaySentence", |a| {
        this(a)?;
        integer(-1)
    })?;
    for name in ["PlayScene", "SetExpression"] {
        def(name, zero)?;
    }
    def("GetExpression", |a| {
        this(a)?;
        Ok(Value::String(a.lua.create_string("")?))
    })?;
    def("ConditionName", |a| {
        this(a)?;
        Ok(Value::String(a.lua.create_string("")?))
    })
}
