//! Players: joining, spawning, dying and leaving with the hooks GMod calls on the way, the
//! ticks of movement through `CUserCmd` and `CMoveData`, the weapons and ammo a player carries,
//! the methods of `Player` and the `player` library.
//!
//! A player joins ([`Runtime::join`](crate::Runtime::join)) as the server takes a client in: `PlayerConnect`, the entity
//! (`OnEntityCreated`), `PlayerAuthed`, `PlayerInitialSpawn`, and the first spawn. Spawning
//! (`ply:Spawn()`) resets health and movement, lets `PlayerSelectSpawn` pick a spawn
//! point and puts the player there, then runs `PlayerSpawn` - where the gamemode sets the
//! speeds, gives the loadout (`Give`, `GiveAmmo`) and the model. Every tick a player runs the
//! command the program last gave ([`Runtime::set_input`](crate::Runtime::set_input)): `StartCommand`, `KeyPress`/`KeyRelease`,
//! then `SetupMove`, `Move` (true skips the engine's movement, [`gmod_movement`]),
//! `FinishMove` (true keeps the result from the player), `PlayerTick`, the active weapon's
//! tick ([`crate::weapon`]) and `PlayerPostThink`. A dead player only gets `StartCommand` and
//! `PlayerDeathThink`, where the gamemode respawns them. Damage takes armor, then
//! health; at none the player dies: `DoPlayerDeath`, `PlayerDeath` (or `PlayerSilentDeath`)
//! and `PostPlayerDeath`, then the weapons go - the active one dropped when it should be.
//! Leaving ([`Runtime::leave`](crate::Runtime::leave)) runs `PlayerDisconnected`.
//!
//! The server has as many slots as the program gives it ([`Runtime::set_max_players`](crate::Runtime::set_max_players)), one
//! by default: a single-player game.
//! Not yet: fall damage, using things (`+use`), vehicles, animations, the client's side of
//! anything (`SendLua` goes to a client that is not there).
//!
//! Until the client is there, the server stands in for the user info a client sends: what the
//! program gave ([`PlayerInfo::user_info`]), then [`DEFAULT_USER_INFO`], then the settings of
//! sandbox's tools (`TOOL.ClientConVar`, which a client makes of the same scripts). A player
//! sets them in their console (`gmod_toolmode weld`), as with a client's own ConVars, and
//! `ply:ConCommand` runs a line as typed by the player.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use gmod_bsp::mask;
use gmod_movement::{Collide, Command, Settings};
use mlua::{AnyUserData, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::damage::{self, DamageInfo};
use crate::entity::{self, Ent, EntityHandle, Kind};
use crate::movedata::{CmdData, MoveData, UserCmd};
use crate::trace::{self, Filter, Sweep};
use crate::vector::{Angle, Vector};
use crate::{convar, host, weapon};

/// The most player slots a server may have.
pub const MAX_PLAYERS: u32 = 128;

/// Player slots: entity indices 1 to this (`game.MaxPlayers`).
pub(crate) fn max_players(lua: &Lua) -> u32 {
    host(lua).game.max_players.get().clamp(1, MAX_PLAYERS)
}

/// Whether a player is the one at the server's own screen: the first, unless the server is
/// dedicated.
pub(crate) fn is_host(lua: &Lua, player: EntityHandle) -> bool {
    player.index == Some(1) && !host(lua).game.dedicated.get()
}

/// The Steam ID 64 of `STEAM_0:0:0`.
const STEAM_ID64_BASE: u64 = 76561197960265728;
/// What the first bot's `SteamID64` is; each bot after it has the next number.
const BOT_STEAM_ID64: u64 = 90071996842377216;

/// `FL_` flags the engine sets on players.
pub mod fl {
    pub const ONGROUND: i64 = 1;
    pub const DUCKING: i64 = 2;
    /// Ducked, for the animation of the player's model.
    pub const ANIMDUCKING: i64 = 4;
    pub const FROZEN: i64 = 64;
    pub const CLIENT: i64 = 256;
    pub const FAKECLIENT: i64 = 512;
    pub const INWATER: i64 = 1024;
    pub const GODMODE: i64 = 32768;
    pub const NOTARGET: i64 = 65536;
}

/// `IN_` keys.
pub mod key {
    pub const ATTACK: u32 = 1;
    pub const JUMP: u32 = 2;
    pub const DUCK: u32 = 4;
    pub const FORWARD: u32 = 8;
    pub const BACK: u32 = 16;
    pub const USE: u32 = 32;
    pub const MOVELEFT: u32 = 512;
    pub const MOVERIGHT: u32 = 1024;
    pub const ATTACK2: u32 = 2048;
    pub const RELOAD: u32 = 8192;
    pub const SPEED: u32 = 131072;
    pub const WALK: u32 = 262144;
    pub const ZOOM: u32 = 524288;
}

const MOVETYPE_NONE: i64 = 0;
const MOVETYPE_WALK: i64 = 2;
const MOVETYPE_NOCLIP: i64 = 8;
const MOVETYPE_OBSERVER: i64 = 10;
const SOLID_BBOX: i64 = 2;
const FSOLID_NOT_SOLID: i64 = 4;
const EF_BONEMERGE: i64 = 1;
const EF_DIMLIGHT: i64 = 4;
const EF_NODRAW: i64 = 32;
/// A player's field of view when the game does not change it, in degrees.
pub const DEFAULT_FOV: f32 = 75.0;
/// `PrintMessage` into the chat.
const HUD_PRINTTALK: i64 = 3;
/// The impulse that toggles the flashlight.
pub const IMPULSE_FLASHLIGHT: u8 = 100;

/// The `DMG_` types armor does not take.
const DMG_NOT_ARMORED: i64 = 32 | 16384 | 131072 | 262144;
/// Armor takes 80% of damage, a point of armor for each two points of damage.
const ARMOR_RATIO: f32 = 0.2;
const ARMOR_BONUS: f32 = 0.5;

/// The user info of a client that has not said otherwise: the client console variables with
/// `FCVAR_USERINFO` that the game reads (`GetInfo`).
pub const DEFAULT_USER_INFO: &[(&str, &str)] = &[
    ("cl_playermodel", "kleiner"),
    ("cl_defaultweapon", "weapon_physgun"),
    ("cl_playercolor", "0.24 0.34 0.41"),
    ("cl_weaponcolor", "0.30 1.80 2.10"),
    ("cl_playerskin", "0"),
    ("cl_playerbodygroups", "0"),
    ("gmod_toolmode", "rope"),
];

/// What the server keeps about players across joins.
#[derive(Default)]
pub(crate) struct Players {
    /// The last `UserID` given: they count from 1.
    pub last_user_id: Cell<i64>,
    /// Bots that have joined, which number their `SteamID64`.
    pub bots: Cell<u64>,
    /// What the server asks of the players' clients, oldest first.
    pub outbox: RefCell<Vec<(EntityHandle, ToClient)>>,
}

/// What the server asks of a player's client beside the entities, the chat and `net`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ToClient {
    /// `SendLua`, `BroadcastLua`: Lua for it to run.
    Lua(String),
    /// `ConCommand`: a line for its console, which sends the server what it does not know.
    Command(String),
    /// `PrintMessage` to the console: a line for it.
    Print(String),
}

/// Asks a player's client something. A bot has none: the server runs its console lines
/// itself, and the rest goes nowhere.
pub(crate) fn to_client(lua: &Lua, player: EntityHandle, what: ToClient) {
    if state(lua, player, |_, p| p.info.bot).unwrap_or(true) {
        if let ToClient::Command(line) = what {
            console(lua, player, &line);
        }
        return;
    }
    host(lua).players.outbox.borrow_mut().push((player, what));
}

/// Who joins: what a client tells the server about itself.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlayerInfo {
    pub name: String,
    /// The player's `SteamID64`. openGMOD's own, made from the key the player proved is
    /// theirs as they connected - no Steam vouches for anyone; bots have none.
    pub steam_id64: Option<u64>,
    pub bot: bool,
    /// `IPAddress`: `loopback` for the player of a single-player game.
    pub address: String,
    /// The client's user info (`cl_playermodel` ...): what [`DEFAULT_USER_INFO`] does not
    /// cover, or covers otherwise.
    pub user_info: Vec<(String, String)>,
    /// The player's client predicts what its commands do - its movement, its weapon - and
    /// hears its weapon as it predicts it ([`predict`]): a client over the network.
    #[serde(default)]
    pub predicts: bool,
}

/// What the engine keeps for a player, beside what every entity has.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlayerState {
    pub info: PlayerInfo,
    pub user_id: i64,
    /// `SteamID64` for a bot, which has no Steam ID of its own.
    pub bot_id: u64,
    pub joined: f64,
    pub alive: bool,
    pub team: i64,
    pub frags: i64,
    pub deaths: i64,
    pub armor: i64,
    pub max_armor: i64,
    /// Where the player looks; the entity faces the same way, upright.
    pub eye_angles: Angle,
    /// How many times the game turned the player itself (a spawn, `SetEyeAngles`): a client
    /// takes the angles the server has then, and otherwise looks where its mouse says.
    pub turned: u32,
    /// What the physics gun holds, as a snapshot carries it to a client, which has no guns
    /// of its own ([`crate::physgun`]).
    pub physgun: EntityHandle,
    /// Movement between ticks: ducking, the eyes, water, ladder. Its origin and velocity are
    /// the entity's.
    pub moving: gmod_movement::Player,
    /// The command the program gave, run every tick until it gives another.
    pub input: UserCmd,
    /// The last command run (`GetCurrentCommand`).
    pub cmd: CmdData,
    pub buttons: u32,
    pub old_buttons: u32,
    pub walk_speed: f32,
    pub run_speed: f32,
    pub slow_walk_speed: f32,
    pub crouched_walk_speed: f32,
    pub duck_speed: f32,
    pub unduck_speed: f32,
    pub jump_power: f32,
    /// The speed this tick's movement allowed: walking, running or walking slowly.
    pub max_speed: f32,
    pub ladder_climb_speed: f32,
    pub step_size: f32,
    pub lagged_movement: f32,
    pub hull: (Vector, Vector),
    pub duck_hull: (Vector, Vector),
    pub view_offset: Vector,
    pub view_offset_ducked: Vector,
    pub weapons: Vec<EntityHandle>,
    pub active_weapon: EntityHandle,
    pub previous_weapon: EntityHandle,
    /// Rounds carried, by ammo type.
    pub ammo: BTreeMap<i64, i64>,
    pub observer_mode: i64,
    pub observer_target: EntityHandle,
    pub class_id: i64,
    pub fov: f32,
    pub can_walk: bool,
    pub can_zoom: bool,
    pub sprint: bool,
    pub avoid_players: bool,
    pub no_collide_teammates: bool,
    pub allow_full_rotation: bool,
    pub allow_weapons_in_vehicle: bool,
    pub should_drop_weapon: bool,
    pub player_color: Vector,
    pub weapon_color: Vector,
    pub hands: EntityHandle,
    pub view_entity: EntityHandle,
    pub ragdoll: EntityHandle,
    pub view_models: [EntityHandle; 3],
    pub driving: EntityHandle,
    pub driving_mode: i64,
    pub hovered_widget: EntityHandle,
    pub pressed_widget: EntityHandle,
    /// What the use key works while it is held (a scripted entity used continuously or on and
    /// off).
    pub using: EntityHandle,
    /// The vehicle the player sits in ([`crate::vehicle`]); how it was turned at the last tick,
    /// to turn the view with it; the collision group the player had before it.
    pub vehicle: EntityHandle,
    pub vehicle_angles: Angle,
    pub vehicle_group: i64,
    /// What the player stands on (`GetGroundEntity`): the world, an entity, or nothing in the
    /// air; and where that entity was, to carry the player along when it moves.
    pub ground: EntityHandle,
    pub ground_at: Option<(Vector, Angle)>,
    pub suit: bool,
    pub suit_power: f32,
    pub view_punch: Angle,
    pub view_punch_velocity: Angle,
    pub render_angles: Angle,
    /// Where the eyes were when the last tick began, to draw between the ticks; none until a
    /// tick after a spawn.
    pub previous_eye: Option<Vector>,
    pub last_hit_group: i64,
    pub activity: i64,
    pub crosshair: bool,
    pub world_clicking_disabled: bool,
    pub suppress_pickup_notices: bool,
    pub draw_view_model: bool,
    pub draw_world_model: bool,
    pub dsp: i64,
    /// Milliseconds to the next footstep, and whether it is the right foot's.
    pub step_time: f32,
    pub step_right: bool,
}

impl PlayerState {
    fn new(info: PlayerInfo, user_id: i64, bot_id: u64, joined: f64) -> PlayerState {
        let settings = Settings::default();
        let vector = |v: [f32; 3]| Vector::new(v[0], v[1], v[2]);
        PlayerState {
            info,
            user_id,
            bot_id,
            joined,
            alive: false,
            team: 0,
            frags: 0,
            deaths: 0,
            armor: 0,
            max_armor: 100,
            eye_angles: Angle::default(),
            turned: 0,
            physgun: EntityHandle::NULL,
            moving: gmod_movement::Player {
                origin: [0.0; 3],
                velocity: [0.0; 3],
                on_ground: false,
                ducked: false,
                view_height: settings.view_height,
                water_level: 0,
                buttons: 0,
                ladder: None,
            },
            input: UserCmd::default(),
            cmd: CmdData::default(),
            buttons: 0,
            old_buttons: 0,
            walk_speed: settings.walk_speed,
            run_speed: settings.run_speed,
            slow_walk_speed: 100.0,
            crouched_walk_speed: settings.crouched_walk_speed,
            duck_speed: settings.duck_speed,
            unduck_speed: settings.unduck_speed,
            jump_power: settings.jump_power,
            max_speed: settings.walk_speed,
            ladder_climb_speed: 200.0,
            step_size: settings.step_size,
            lagged_movement: 1.0,
            hull: (vector(settings.hull.0), vector(settings.hull.1)),
            duck_hull: (vector(settings.duck_hull.0), vector(settings.duck_hull.1)),
            view_offset: Vector::new(0.0, 0.0, settings.view_height),
            view_offset_ducked: Vector::new(0.0, 0.0, settings.duck_view_height),
            weapons: Vec::new(),
            active_weapon: EntityHandle::NULL,
            previous_weapon: EntityHandle::NULL,
            ammo: BTreeMap::new(),
            observer_mode: 0,
            observer_target: EntityHandle::NULL,
            class_id: 0,
            fov: DEFAULT_FOV,
            can_walk: true,
            can_zoom: true,
            sprint: true,
            avoid_players: true,
            no_collide_teammates: false,
            allow_full_rotation: false,
            allow_weapons_in_vehicle: false,
            should_drop_weapon: false,
            player_color: Vector::new(0.24, 0.34, 0.41),
            weapon_color: Vector::new(0.30, 1.80, 2.10),
            hands: EntityHandle::NULL,
            view_entity: EntityHandle::NULL,
            ragdoll: EntityHandle::NULL,
            view_models: [EntityHandle::NULL; 3],
            driving: EntityHandle::NULL,
            driving_mode: 0,
            hovered_widget: EntityHandle::NULL,
            pressed_widget: EntityHandle::NULL,
            using: EntityHandle::NULL,
            vehicle: EntityHandle::NULL,
            vehicle_angles: Angle::default(),
            vehicle_group: 0,
            ground: EntityHandle::NULL,
            ground_at: None,
            suit: true,
            suit_power: 100.0,
            view_punch: Angle::default(),
            view_punch_velocity: Angle::default(),
            render_angles: Angle::default(),
            previous_eye: None,
            last_hit_group: 0,
            activity: 0,
            crosshair: true,
            world_clicking_disabled: false,
            suppress_pickup_notices: false,
            draw_view_model: true,
            draw_world_model: true,
            dsp: 0,
            step_time: 0.0,
            step_right: false,
        }
    }

    /// `SteamID64`: the player's ID, the bot's number, or 0 for a player with none.
    fn steam_id64(&self) -> u64 {
        if self.info.bot { self.bot_id } else { self.info.steam_id64.unwrap_or(0) }
    }

    /// `SteamID`: `STEAM_0:Y:Z`, `BOT`, or `STEAM_ID_PENDING` while there is none.
    fn steam_id(&self) -> String {
        match (self.info.bot, self.info.steam_id64) {
            (true, _) => "BOT".to_string(),
            (false, Some(id)) if id >= STEAM_ID64_BASE => {
                let account = id - STEAM_ID64_BASE;
                format!("STEAM_0:{}:{}", account % 2, account / 2)
            }
            _ => "STEAM_ID_PENDING".to_string(),
        }
    }

    /// `UniqueID`: 1 in a single-player game, else the CRC of `gm_<SteamID>_gm`.
    fn unique_id(&self, single_player: bool) -> u32 {
        if single_player { 1 } else { crc32(format!("gm_{}_gm", self.steam_id()).as_bytes()) }
    }

    fn user_info(&self, name: &str) -> Option<String> {
        if name.eq_ignore_ascii_case("name") {
            return Some(self.info.name.clone());
        }
        let own = self.info.user_info.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
        own.or_else(|| DEFAULT_USER_INFO.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.to_string()))
    }
}

/// CRC-32 (IEEE), as `util.CRC` computes it.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

fn vector(v: [f32; 3]) -> Vector {
    Vector::new(v[0], v[1], v[2])
}

/// What a player's state says, if the entity is a player.
pub(crate) fn state<R>(lua: &Lua, handle: EntityHandle, f: impl FnOnce(&Ent, &PlayerState) -> R) -> Option<R> {
    let host = host(lua);
    let ent = host.entities.get(handle)?;
    let player = ent.player.as_deref()?;
    Some(f(&ent, player))
}

/// Changes a player and its entity.
pub(crate) fn with_player<R>(lua: &Lua, handle: EntityHandle, f: impl FnOnce(&mut Ent, &mut PlayerState) -> R) -> Option<R> {
    host(lua)
        .entities
        .with_mut(handle, |ent| {
            let mut player = ent.player.take()?;
            let result = f(ent, &mut player);
            ent.player = Some(player);
            Some(result)
        })
        .flatten()
}

fn is_player(lua: &Lua, handle: EntityHandle) -> bool {
    state(lua, handle, |_, _| ()).is_some()
}

fn userdata(lua: &Lua, handle: EntityHandle) -> Option<AnyUserData> {
    match entity::push(lua, handle) {
        Ok(Value::UserData(ud)) if host(lua).entities.is_valid(handle) => Some(ud),
        _ => None,
    }
}

fn ent_value(lua: &Lua, handle: EntityHandle) -> Value {
    entity::push(lua, handle).unwrap_or(Value::Nil)
}

fn hook(lua: &Lua, name: &str, args: impl IntoIterator<Item = Value>) -> MultiValue {
    crate::call_hook(lua, name, MultiValue::from_iter(args))
}

fn truthy(values: &MultiValue) -> bool {
    !matches!(values.front(), None | Some(Value::Nil) | Some(Value::Boolean(false)))
}

/// Every player there is, in index order.
pub(crate) fn all(lua: &Lua) -> Vec<EntityHandle> {
    let entities = &host(lua).entities;
    (1..=max_players(lua))
        .filter_map(|i| entities.at(i))
        .filter(|&h| entities.get(h).is_some_and(|ent| ent.player.is_some() && !ent.removed))
        .collect()
}

/// The world as a player's movement sees it: the map, and the solid entities but the player.
/// What the movement ran into is kept: the entity, and the normal of where it was hit.
struct Space<'a> {
    lua: &'a Lua,
    player: EntityHandle,
    touched: RefCell<Vec<(EntityHandle, Vector)>>,
}

impl<'a> Space<'a> {
    fn new(lua: &'a Lua, player: EntityHandle) -> Space<'a> {
        Space { lua, player, touched: RefCell::new(Vec::new()) }
    }
}

impl Collide for Space<'_> {
    fn trace_mask(&self, start: [f32; 3], end: [f32; 3], mins: [f32; 3], maxs: [f32; 3], mask: u32) -> gmod_bsp::Trace {
        let sweep = Sweep {
            start: vector(start),
            end: vector(end),
            mins: vector(mins),
            maxs: vector(maxs),
            mask,
            // what the player carries they walk through
            filter: Filter::Entities(vec![self.player, crate::pickup::carrying(self.lua, self.player).unwrap_or(EntityHandle::NULL)]),
            whitelist: false,
            ignore_world: false,
            group: trace::group::PLAYER_MOVEMENT,
        };
        let hit = trace::trace(self.lua, &sweep);
        if hit.fraction < 1.0 && hit.entity.index.is_some_and(|i| i != 0) {
            self.touched.borrow_mut().push((hit.entity, hit.normal));
        }
        gmod_bsp::Trace {
            fraction: hit.fraction,
            end: array(hit.end),
            normal: array(hit.normal),
            start_solid: hit.start_solid,
            all_solid: hit.all_solid,
            fraction_left_solid: hit.fraction_left_solid,
            contents: hit.contents,
            texinfo: None,
            surface_flags: hit.surface_flags,
            displacement: hit.displacement,
            model: None,
            static_prop: None,
        }
    }

    fn contents(&self, point: [f32; 3]) -> u32 {
        trace::point_contents(self.lua, vector(point))
    }
}

/// What movement goes by for a player: the console variables and the player's speeds, with
/// `speed` for walking and running alike (the choice between them is made before).
fn settings(lua: &Lua, p: &PlayerState, speed: f32) -> Settings {
    let cvar = |name: &str, default: f32| convar::value(lua, name).map_or(default, |v| convar::atof(&v) as f32);
    Settings {
        gravity: cvar("sv_gravity", 600.0),
        friction: cvar("sv_friction", 8.0),
        stop_speed: cvar("sv_stopspeed", 10.0),
        accelerate: cvar("sv_accelerate", 10.0),
        air_accelerate: cvar("sv_airaccelerate", 10.0),
        max_velocity: cvar("sv_maxvelocity", 3500.0),
        walk_speed: speed,
        run_speed: speed,
        crouched_walk_speed: p.crouched_walk_speed,
        duck_speed: p.duck_speed,
        unduck_speed: p.unduck_speed,
        jump_power: p.jump_power,
        step_size: p.step_size,
        hull: (array(p.hull.0), array(p.hull.1)),
        duck_hull: (array(p.duck_hull.0), array(p.duck_hull.1)),
        view_height: p.view_offset.z,
        duck_view_height: p.view_offset_ducked.z,
        noclip_speed: cvar("sv_noclipspeed", 5.0),
        noclip_accelerate: cvar("sv_noclipaccelerate", 5.0),
    }
}

/// How hard the view springs back to where the player looks, and how much it is held back.
const PUNCH_SPRING: f32 = 65.0;
const PUNCH_DAMPING: f32 = 9.0;

/// The view kicks (`ViewPunch`): a recoil, a blow.
pub(crate) fn view_punch(lua: &Lua, handle: EntityHandle, punch: Angle) {
    with_player(lua, handle, |_, p| {
        let v = p.view_punch_velocity;
        p.view_punch_velocity = Angle::new(v.p + punch.p * 20.0, v.y + punch.y * 20.0, v.r + punch.r * 20.0);
    });
}

/// A tick of the view's kick: it moves on, and springs back to nothing.
fn decay_punch(lua: &Lua, handle: EntityHandle) {
    let dt = crate::timer::TICK_INTERVAL as f32;
    with_player(lua, handle, |_, p| {
        let (punch, velocity) = (p.view_punch, p.view_punch_velocity);
        if punch == Angle::default() && velocity == Angle::default() {
            return;
        }
        let damping = (1.0 - PUNCH_DAMPING * dt).max(0.0);
        let axis = |a: f32, v: f32, limit: f32| {
            let a = (a + v * dt).clamp(-limit, limit);
            let v = v * damping - a * PUNCH_SPRING * dt;
            // still enough is still
            if a.abs() < 0.001 && v.abs() < 0.001 { (0.0, 0.0) } else { (a, v) }
        };
        let (pp, vp) = axis(punch.p, velocity.p, 89.0);
        let (py, vy) = axis(punch.y, velocity.y, 179.0);
        let (pr, vr) = axis(punch.r, velocity.r, 89.0);
        p.view_punch = Angle::new(pp, py, pr);
        p.view_punch_velocity = Angle::new(vp, vy, vr);
    });
}

/// The hit group the last bullet that hit a player hit (`LastHitGroup`).
pub(crate) fn set_last_hit_group(lua: &Lua, handle: EntityHandle, group: i64) {
    with_player(lua, handle, |_, p| p.last_hit_group = group);
}

/// The eyes of a player and where they aim.
pub(crate) fn eye(lua: &Lua, handle: EntityHandle) -> Option<(Vector, Vector)> {
    let vehicle = crate::vehicle::of(lua, handle);
    if vehicle.index.is_some()
        && let Some((eyes, _)) = crate::vehicle::eyes(lua, vehicle)
    {
        return state(lua, handle, |_, p| (eyes, p.eye_angles.vectors().0));
    }
    let (pos, _) = host(lua).entities.transform(handle)?;
    state(lua, handle, |_, p| (pos + Vector::new(0.0, 0.0, p.moving.view_height), p.eye_angles.vectors().0))
}

pub(crate) fn eye_angles(lua: &Lua, handle: EntityHandle) -> Option<Angle> {
    state(lua, handle, |_, p| p.eye_angles)
}

/// The weapon in a player's hands (`NULL` for none); none for what is not a player.
pub(crate) fn active_weapon(lua: &Lua, handle: EntityHandle) -> Option<EntityHandle> {
    state(lua, handle, |_, p| p.active_weapon)
}

pub(crate) fn alive(lua: &Lua, handle: EntityHandle) -> bool {
    state(lua, handle, |_, p| p.alive).unwrap_or(false)
}

pub(crate) fn water_level(lua: &Lua, handle: EntityHandle) -> Option<u8> {
    state(lua, handle, |_, p| p.moving.water_level)
}

/// A player joins: `PlayerConnect`, the entity, `PlayerAuthed`, `PlayerInitialSpawn` and the
/// first spawn. None when every slot is taken.
pub(crate) fn join(lua: &Lua, info: PlayerInfo) -> Option<EntityHandle> {
    let index = (1..=max_players(lua)).find(|&i| host(lua).entities.at(i).is_none())?;
    let address =
        if info.address.is_empty() { if info.bot { "BOT".to_string() } else { "loopback".to_string() } } else { info.address.clone() };
    let info = PlayerInfo { address, ..info };
    let name = info.name.clone();
    if let (Ok(n), Ok(a)) = (lua.create_string(&name), lua.create_string(&info.address)) {
        hook(lua, "PlayerConnect", [Value::String(n), Value::String(a)]);
    }
    crate::chat::join_leave(lua, EntityHandle::NULL, &name, &format!("Player {name} has joined the game"));
    let handle = {
        let host = host(lua);
        let now = host.clock.cur_time.get();
        let user_id = host.players.last_user_id.get() + 1;
        host.players.last_user_id.set(user_id);
        let bot_id = BOT_STEAM_ID64 + host.players.bots.get();
        if info.bot {
            host.players.bots.set(host.players.bots.get() + 1);
        }
        let bot = info.bot;
        let mut ent = Ent::new("player", Kind::Player, host.entities.next_creation_id(), now);
        ent.player = Some(Box::new(PlayerState::new(info, user_id, bot_id, now)));
        ent.flags = fl::CLIENT | if bot { fl::FAKECLIENT } else { 0 };
        ent.spawned = true;
        let (mins, maxs) = ent.player.as_ref().map(|p| p.hull).expect("set above");
        (ent.mins, ent.maxs) = (mins, maxs);
        host.entities.insert(ent, index)?
    };
    crate::ents::announce(lua, handle);
    let ply = ent_value(lua, handle);
    let single = max_players(lua) == 1;
    let (bot, steam_id, unique_id) = state(lua, handle, |_, p| (p.info.bot, p.steam_id(), p.unique_id(single).to_string()))?;
    if !bot && let (Ok(s), Ok(u)) = (lua.create_string(&steam_id), lua.create_string(&unique_id)) {
        hook(lua, "PlayerAuthed", [ply.clone(), Value::String(s), Value::String(u)]);
    }
    hook(lua, "PlayerInitialSpawn", [ply, Value::Boolean(false)]);
    if host(lua).entities.is_valid(handle) {
        spawn(lua, handle);
    }
    Some(handle)
}

/// The view models of a player (`GetViewModel`), made at the first spawn.
fn make_view_models(lua: &Lua, handle: EntityHandle) {
    let missing = state(lua, handle, |_, p| p.view_models.iter().any(|&vm| !host(lua).entities.is_valid(vm))).unwrap_or(false);
    if !missing {
        return;
    }
    let mut made = [EntityHandle::NULL; 3];
    for slot in &mut made {
        if let Ok(Some(vm)) = crate::ents::create_engine(lua, "predicted_viewmodel", Kind::Entity) {
            host(lua).entities.with_mut(vm, |ent| ent.owner = handle);
            entity::set_parent(lua, vm, handle);
            *slot = vm;
        }
    }
    with_player(lua, handle, |_, p| p.view_models = made);
}

/// Puts a player at a point, a little higher when there is no room there.
fn place(lua: &Lua, handle: EntityHandle, pos: Vector) {
    let Some(settings) = state(lua, handle, |_, p| settings(lua, p, p.walk_speed)) else { return };
    let space = Space::new(lua, handle);
    let origin = gmod_movement::Player::spawn(&space, &settings, array(pos)).map_or(pos, |p| vector(p.origin));
    entity::set_world_pos(lua, handle, origin);
}

/// `ply:Spawn()`: alive again, with full health, standing; `PlayerSelectSpawn` says where,
/// then `PlayerSpawn` runs.
pub(crate) fn spawn(lua: &Lua, handle: EntityHandle) {
    let reset = with_player(lua, handle, |ent, p| {
        p.alive = true;
        ent.health = 100;
        ent.max_health = 100;
        p.armor = 0;
        ent.move_type = MOVETYPE_WALK;
        ent.solid = SOLID_BBOX;
        ent.solid_flags = 0;
        ent.collision_group = trace::group::PLAYER;
        ent.flags &= !(fl::ONGROUND | fl::DUCKING | fl::INWATER);
        ent.effects &= !EF_NODRAW;
        ent.velocity = Vector::default();
        (ent.mins, ent.maxs) = p.hull;
        p.observer_mode = 0;
        p.observer_target = EntityHandle::NULL;
        p.moving.ducked = false;
        p.moving.view_height = p.view_offset.z;
        p.moving.velocity = [0.0; 3];
        p.moving.ladder = None;
        p.view_punch = Angle::default();
        p.view_punch_velocity = Angle::default();
        p.fov = DEFAULT_FOV;
        p.previous_eye = None;
    });
    if reset.is_none() {
        return;
    }
    make_view_models(lua, handle);
    let ply = ent_value(lua, handle);
    let spot = hook(lua, "PlayerSelectSpawn", [ply.clone(), Value::Boolean(false)]);
    let spot = entity::handle_of(spot.front()).filter(|&h| host(lua).entities.is_valid(h));
    if let Some((pos, angles)) = spot.and_then(|spot| host(lua).entities.transform(spot)) {
        place(lua, handle, pos);
        let eye = Angle::new(angles.p, angles.y, 0.0);
        entity::set_world_angles(lua, handle, Angle::new(0.0, angles.y, 0.0));
        with_player(lua, handle, |_, p| {
            p.eye_angles = eye;
            p.input.view_angles = eye;
            p.turned = p.turned.wrapping_add(1);
        });
    }
    hook(lua, "PlayerSpawn", [ply, Value::Boolean(false)]);
}

/// How a player dies.
pub(crate) struct Death {
    pub info: DamageInfo,
    /// The `CTakeDamageInfo` the damage came in, handed on to `DoPlayerDeath`.
    pub userdata: Option<AnyUserData>,
    /// `KillSilent`: `PlayerSilentDeath` rather than `DoPlayerDeath` and `PlayerDeath`.
    pub silent: bool,
}

/// A player dies (if alive): the death hooks, then the weapons go and the ammo with them.
pub(crate) fn kill(lua: &Lua, handle: EntityHandle, death: Death) {
    let now = host(lua).clock.cur_time.get();
    let alive = with_player(lua, handle, |ent, p| {
        let alive = p.alive;
        p.alive = false;
        ent.health = ent.health.min(0);
        ent.velocity = Vector::default();
        p.moving.velocity = [0.0; 3];
        let _ = now;
        alive
    });
    if alive != Some(true) {
        return;
    }
    let ply = ent_value(lua, handle);
    if death.silent {
        hook(lua, "PlayerSilentDeath", [ply.clone()]);
    } else {
        let info = match death.userdata {
            Some(ud) => Value::UserData(ud),
            None => damage::push(lua, death.info).unwrap_or(Value::Nil),
        };
        let attacker = ent_value(lua, death.info.attacker.unwrap_or(EntityHandle::NULL));
        let inflictor = ent_value(lua, death.info.inflictor.unwrap_or(EntityHandle::NULL));
        hook(lua, "DoPlayerDeath", [ply.clone(), attacker.clone(), info]);
        hook(lua, "PlayerDeath", [ply.clone(), inflictor, attacker]);
    }
    hook(lua, "PostPlayerDeath", [ply]);
    // the active weapon falls when it should, the rest go
    let (active, drop) = state(lua, handle, |_, p| (p.active_weapon, p.should_drop_weapon)).unwrap_or((EntityHandle::NULL, false));
    if drop && host(lua).entities.is_valid(active) {
        drop_weapon(lua, handle, active, None);
    }
    strip_weapons(lua, handle);
    with_player(lua, handle, |_, p| p.ammo.clear());
}

/// Damage to a living player, after the `EntityTakeDamage` hook let it through: not in god
/// mode, and only if `PlayerShouldTakeDamage` does not say no. Armor takes its part; the
/// player is hurt (`PlayerHurt`) and dies at no health.
pub(crate) fn hurt(lua: &Lua, handle: EntityHandle, info: DamageInfo, userdata: &AnyUserData) {
    let Some((alive, god)) = state(lua, handle, |ent, p| (p.alive, ent.flags & fl::GODMODE != 0)) else { return };
    if !alive || god {
        return;
    }
    let ply = ent_value(lua, handle);
    let attacker = ent_value(lua, info.attacker.unwrap_or(EntityHandle::NULL));
    let allowed = hook(lua, "PlayerShouldTakeDamage", [ply.clone(), attacker.clone()]);
    if matches!(allowed.front(), Some(Value::Boolean(false))) {
        return;
    }
    let mut amount = info.damage;
    let (health, taken) = with_player(lua, handle, |ent, p| {
        if p.armor > 0 && info.kind & DMG_NOT_ARMORED == 0 {
            let to_health = amount * ARMOR_RATIO;
            let mut armor = (amount - to_health) * ARMOR_BONUS;
            if armor > p.armor as f32 {
                armor = p.armor as f32;
                amount -= armor / ARMOR_BONUS;
                p.armor = 0;
            } else {
                p.armor -= armor as i64;
                amount = to_health;
            }
        }
        let before = ent.health;
        ent.health = (ent.health as f32 - amount) as i64;
        (ent.health, before - ent.health)
    })
    .unwrap_or((0, 0));
    hook(lua, "PlayerHurt", [ply, attacker, Value::Integer(health), Value::Number(f64::from(amount.max(taken as f32)))]);
    if health <= 0 {
        kill(lua, handle, Death { info, userdata: Some(userdata.clone()), silent: false });
    }
}

/// A player leaves: `PlayerDisconnected`, then their weapons and the player go.
pub(crate) fn leave(lua: &Lua, handle: EntityHandle) {
    if !is_player(lua, handle) {
        return;
    }
    hook(lua, "PlayerDisconnected", [ent_value(lua, handle)]);
    let name = state(lua, handle, |_, p| p.info.name.clone()).unwrap_or_default();
    crate::chat::join_leave(lua, handle, &name, &format!("Player {name} left the game (Disconnect by user.)"));
    strip_weapons(lua, handle);
    let view_models = state(lua, handle, |_, p| p.view_models).unwrap_or([EntityHandle::NULL; 3]);
    for vm in view_models {
        crate::ents::remove(lua, vm);
    }
    crate::ents::remove(lua, handle);
}

/// The command a player sends from now on, every tick; its impulse, mouse wheel and mouse
/// movement only with the next one, added to what the next one has not sent yet.
pub(crate) fn set_input(lua: &Lua, handle: EntityHandle, cmd: UserCmd) {
    with_player(lua, handle, |_, p| {
        let waiting = p.input;
        p.input = UserCmd {
            impulse: if cmd.impulse != 0 { cmd.impulse } else { waiting.impulse },
            mouse_wheel: waiting.mouse_wheel + cmd.mouse_wheel,
            mouse_x: waiting.mouse_x + cmd.mouse_x,
            mouse_y: waiting.mouse_y + cmd.mouse_y,
            ..cmd
        };
    });
}

/// One tick of every player.
pub(crate) fn tick_all(lua: &Lua) {
    for handle in all(lua) {
        tick(lua, handle);
    }
}

/// One tick of a player: the command, movement, the weapon.
fn tick(lua: &Lua, handle: EntityHandle) {
    let tick = host(lua).clock.ticks.get() as i64;
    decay_punch(lua, handle);
    if let Some((eye, _)) = eye(lua, handle) {
        with_player(lua, handle, |_, p| p.previous_eye = Some(eye));
    }
    let Some(cmd) = with_player(lua, handle, |_, p| {
        p.cmd = CmdData { cmd: p.input, number: p.cmd.number + 1, tick, select: None };
        // an impulse, the mouse wheel and the mouse go with one command, not every one after it
        p.input.impulse = 0;
        p.input.mouse_wheel = 0;
        p.input.mouse_x = 0;
        p.input.mouse_y = 0;
        p.cmd
    }) else {
        return;
    };
    let Some(ply) = userdata(lua, handle) else { return };
    let Ok(cmd_ud) = lua.create_userdata(cmd) else { return };
    hook(lua, "StartCommand", [Value::UserData(ply.clone()), Value::UserData(cmd_ud.clone())]);
    let cmd = cmd_ud.borrow::<CmdData>().map(|c| *c).unwrap_or(cmd);
    // the mouse turns what the physics gun holds while E is down, not the view
    let turning = cmd.cmd.buttons & key::USE != 0 && crate::physgun::holding(lua, handle).is_some();
    let Some((pressed, released, alive, frozen)) = with_player(lua, handle, |ent, p| {
        p.cmd = cmd;
        p.old_buttons = p.buttons;
        p.buttons = cmd.cmd.buttons;
        let frozen = ent.flags & fl::FROZEN != 0;
        if !frozen && !turning {
            p.eye_angles = cmd.cmd.view_angles;
        }
        (p.buttons & !p.old_buttons, p.old_buttons & !p.buttons, p.alive, frozen)
    }) else {
        return;
    };
    for bit in (0..32).map(|i| 1u32 << i) {
        if pressed & bit != 0 {
            hook(lua, "KeyPress", [Value::UserData(ply.clone()), Value::Integer(bit.into())]);
        }
        if released & bit != 0 {
            hook(lua, "KeyRelease", [Value::UserData(ply.clone()), Value::Integer(bit.into())]);
        }
    }
    if !alive {
        hook(lua, "PlayerDeathThink", [Value::UserData(ply)]);
        return;
    }
    if let Some(index) = cmd.select
        && let Some(weapon) = host(lua).entities.at(index)
    {
        switch_weapon(lua, handle, weapon);
    }
    if cmd.cmd.impulse == IMPULSE_FLASHLIGHT {
        let on = state(lua, handle, |_, _| ()).and_then(|_| host(lua).entities.get(handle).map(|ent| ent.effects & EF_DIMLIGHT != 0));
        let wanted = !on.unwrap_or(false);
        if truthy(&hook(lua, "PlayerSwitchFlashlight", [Value::UserData(ply.clone()), Value::Boolean(wanted)])) {
            set_flashlight(lua, handle, wanted);
        }
    }

    // in a vehicle: its seat, not walking, and no weapon
    if state(lua, handle, |_, p| p.vehicle.index.is_some()).unwrap_or(false) {
        let mv = movedata(lua, handle, &cmd, false).and_then(|mv| lua.create_userdata(mv).ok());
        if crate::vehicle::tick(lua, handle, &ply, mv, pressed) {
            crate::anim::animate_player(lua, handle);
            hook(lua, "PlayerPostThink", [Value::UserData(ply)]);
            return;
        }
    }

    // movement
    let Some(mut mv) = movedata(lua, handle, &cmd, frozen) else { return };
    let Ok(mv_ud) = lua.create_userdata(mv) else { return };
    hook(lua, "SetupMove", [Value::UserData(ply.clone()), Value::UserData(mv_ud.clone()), Value::UserData(cmd_ud)]);
    let skip = truthy(&hook(lua, "Move", [Value::UserData(ply.clone()), Value::UserData(mv_ud.clone())]));
    if !skip {
        mv = mv_ud.borrow::<MoveData>().map(|m| *m).unwrap_or(mv);
        engine_move(lua, handle, &mut mv);
        if let Ok(mut m) = mv_ud.borrow_mut::<MoveData>() {
            *m = mv;
        }
    }
    let keep = truthy(&hook(lua, "FinishMove", [Value::UserData(ply.clone()), Value::UserData(mv_ud.clone())]));
    let grounded = host(lua).entities.get(handle).is_some_and(|ent| ent.flags & fl::ONGROUND != 0);
    if !keep {
        let mv = mv_ud.borrow::<MoveData>().map(|m| *m).unwrap_or(mv);
        finish_move(lua, handle, &mv);
    }
    // a jump off the ground is the jump of the player's model
    let jumped = host(lua).entities.get(handle).is_some_and(|ent| ent.flags & fl::ONGROUND == 0 && ent.velocity.z > 0.0);
    if grounded && jumped && cmd.cmd.buttons & key::JUMP != 0 {
        crate::anim::animation_event(lua, handle, crate::anim::EVENT_JUMP, 0);
    }
    crate::anim::animate_player(lua, handle);
    footsteps(lua, handle, &ply);
    hook(lua, "PlayerTick", [Value::UserData(ply.clone()), Value::UserData(mv_ud)]);

    // the weapon in hand
    let active = state(lua, handle, |_, p| p.active_weapon).unwrap_or(EntityHandle::NULL);
    let (mut buttons, mut pressed) = state(lua, handle, |_, p| (p.buttons, p.buttons & !p.old_buttons)).unwrap_or_default();
    // carrying something, the attack key throws it and the weapon is still
    if crate::pickup::carrying(lua, handle).is_some() {
        // what the gravity gun holds it launches or lets go; what the hands hold is thrown
        if !crate::physcannon::holding(lua, handle, active, pressed) && pressed & IN_ATTACK != 0 {
            crate::pickup::drop(lua, handle, true);
        }
        buttons &= !(IN_ATTACK | IN_ATTACK2);
        pressed &= !(IN_ATTACK | IN_ATTACK2);
    }
    if host(lua).entities.is_valid(active) {
        // a client that predicted the weapon heard it then: what it makes heard now is the
        // others' (`SuppressHostEvents`)
        let predicts = state(lua, handle, |_, p| p.info.predicts).unwrap_or(false);
        let before = predicts.then(|| host(lua).sounds.unheard_by.replace(Some(handle)));
        weapon::post_frame(lua, active, buttons, pressed);
        if let Some(before) = before {
            host(lua).sounds.unheard_by.set(before);
        }
        crate::physgun::command(lua, handle, active, &UserCmd { buttons, ..cmd.cmd }, pressed);
    }
    if host(lua).entities.is_valid(handle) && self::alive(lua, handle) {
        use_key(lua, handle, buttons, pressed);
    }
    if host(lua).entities.is_valid(handle) {
        hook(lua, "PlayerPostThink", [Value::UserData(ply)]);
    }
}

/// The `CMoveData` a tick starts from: where the player is and what the command asks.
fn movedata(lua: &Lua, handle: EntityHandle, cmd: &CmdData, frozen: bool) -> Option<MoveData> {
    let (pos, _) = host(lua).entities.transform(handle)?;
    with_player(lua, handle, |ent, p| {
        let c = &cmd.cmd;
        let speed = if c.buttons & key::SPEED != 0 && p.sprint {
            p.run_speed
        } else if c.buttons & key::WALK != 0 && p.can_walk {
            p.slow_walk_speed
        } else {
            p.walk_speed
        };
        p.max_speed = speed;
        let (forward, side, up) = if frozen { (0.0, 0.0, 0.0) } else { (c.forward_move, c.side_move, c.up_move) };
        MoveData {
            origin: pos,
            velocity: ent.velocity,
            angles: p.eye_angles,
            old_angles: p.eye_angles,
            move_angles: p.eye_angles,
            abs_move_angles: p.eye_angles,
            buttons: if frozen { c.buttons & !(key::JUMP | key::DUCK) } else { c.buttons },
            old_buttons: p.old_buttons,
            forward,
            side,
            up,
            max_speed: speed,
            max_client_speed: speed,
            impulse: c.impulse,
            ..MoveData::default()
        }
    })
}

/// The engine's movement of a tick, on the `CMoveData`: walking, swimming and climbing for
/// `MOVETYPE_WALK`, flying through everything for noclip and observers, nothing otherwise.
fn engine_move(lua: &Lua, handle: EntityHandle, mv: &mut MoveData) {
    let Some((move_type, mut moving, settings, lag)) = state(lua, handle, |ent, p| {
        let speed = if mv.max_client_speed > 0.0 { mv.max_client_speed.min(mv.max_speed) } else { mv.max_speed };
        (ent.move_type, p.moving, settings(lua, p, speed), p.lagged_movement)
    }) else {
        return;
    };
    let seconds = crate::timer::TICK_INTERVAL as f32 * lag;
    if move_type == MOVETYPE_WALK {
        mv.origin = carried(lua, handle, mv.origin, &moving, &settings);
    }
    moving.origin = array(mv.origin);
    moving.velocity = array(mv.velocity);
    let command =
        Command { angles: [mv.angles.p, mv.angles.y, mv.angles.r], forward_move: mv.forward, side_move: mv.side, buttons: mv.buttons };
    let space = Space::new(lua, handle);
    match move_type {
        MOVETYPE_WALK => moving.tick(&space, &settings, &command, seconds),
        MOVETYPE_NOCLIP | MOVETYPE_OBSERVER => moving.noclip(&settings, &command, seconds),
        _ => return,
    }
    mv.origin = vector(moving.origin);
    mv.velocity = vector(moving.velocity);
    with_player(lua, handle, |_, p| p.moving = moving);
    if move_type == MOVETYPE_WALK {
        push_props(lua, mv, &space.touched.borrow());
        find_ground(lua, handle, &moving, &settings);
    } else {
        with_player(lua, handle, |_, p| (p.ground, p.ground_at) = (EntityHandle::NULL, None));
    }
}

/// A client moves its own player on from where the server last said it was (at `from`, the
/// time of the newest snapshot), as the server will once the commands it has not answered yet
/// arrive: each for as long as it was the player's, in the server's ticks - the engine's
/// movement, without the gamemode's hooks, which mostly leave it be, and the weapon in hand
/// (its `Think`, and what the keys held fire), each tick at the `CurTime` the server will run
/// it at. `played` is how long the commands before these were the player's, all told: a tick
/// that ends no further into the commands than one before did is predicted again - every
/// frame, until the server's answer comes - and not for the first time
/// (`IsFirstTimePredicted`): what it makes heard is heard once. What the server then says
/// puts it right. The view model plays on to the time of the newest command. How far that
/// time goes into a tick not run yet, 0 to 1: the eyes are drawn that far from where they were
/// before the last tick ([`PlayerState::previous_eye`]) to where they are.
pub(crate) fn predict(lua: &Lua, handle: EntityHandle, commands: &[(UserCmd, f32)], from: f64, played: f64) -> f32 {
    let tick = crate::timer::TICK_INTERVAL as f32;
    let still = eye(lua, handle).map(|(eye, _)| eye);
    with_player(lua, handle, |_, p| p.previous_eye = still);
    // the time each tick falls in, from the oldest command on
    let mut time = 0.0;
    let mut until = 0.0;
    let mut commands = commands.iter();
    let Some(&(mut cmd, first)) = commands.next() else { return 0.0 };
    until += first;
    let total: f32 = until + commands.clone().map(|(_, s)| s).sum::<f32>();
    let clock = &host(lua).clock;
    let outside = clock.cur_time.get();
    clock.unpredicted.set(Some(outside));
    // the server's ticks after the one of the snapshot
    let base = (from / crate::timer::TICK_INTERVAL + 1e-6).floor();
    let mut ticks = 0.0;
    let fraction = loop {
        if time + tick > total + 1e-4 {
            break ((total - time) / tick).clamp(0.0, 1.0);
        }
        while time >= until - 1e-4
            && let Some(&(next, seconds)) = commands.next()
        {
            cmd = next;
            until += seconds;
        }
        time += tick;
        ticks += 1.0;
        clock.cur_time.set((base + ticks) * crate::timer::TICK_INTERVAL);
        let reached = played + f64::from(time);
        clock.first_prediction.set(reached > clock.predicted_until.get() + 1e-4);
        if !predict_tick(lua, handle, &cmd) {
            break 0.0;
        }
        clock.predicted_until.set(clock.predicted_until.get().max(reached));
    };
    // what is before the eyes plays on to the time of the newest command
    clock.cur_time.set(base * crate::timer::TICK_INTERVAL + f64::from(total));
    for vm in crate::anim::view_models_of(lua, handle) {
        crate::anim::frame_advance(lua, vm, None);
    }
    clock.cur_time.set(outside);
    clock.unpredicted.set(None);
    fraction
}

/// A tick of a client's own player, predicted: the command's movement, and the weapon in
/// hand. Whether the player is there to go on with.
fn predict_tick(lua: &Lua, handle: EntityHandle, cmd: &UserCmd) -> bool {
    let Some((alive, frozen, turning)) =
        state(lua, handle, |ent, p| (p.alive, ent.flags & fl::FROZEN != 0, cmd.buttons & key::USE != 0 && p.physgun.index.is_some()))
    else {
        return false;
    };
    if !alive {
        return false;
    }
    let before = eye(lua, handle).map(|(eye, _)| eye);
    let Some((buttons, pressed, active)) = with_player(lua, handle, |_, p| {
        p.previous_eye = before;
        if !frozen && !turning {
            p.eye_angles = cmd.view_angles;
        }
        p.old_buttons = p.buttons;
        p.buttons = cmd.buttons;
        (p.buttons, p.buttons & !p.old_buttons, p.active_weapon)
    }) else {
        return false;
    };
    decay_punch(lua, handle);
    let data = CmdData { cmd: *cmd, ..CmdData::default() };
    let Some(mut mv) = movedata(lua, handle, &data, frozen) else { return false };
    engine_move(lua, handle, &mut mv);
    finish_move(lua, handle, &mv);
    if host(lua).entities.is_valid(active) && host(lua).entities.get(active).is_some_and(|w| !w.dormant) {
        weapon::post_frame(lua, active, buttons, pressed);
    }
    host(lua).entities.is_valid(handle)
}

/// How heavy a player is to what they push, in kilograms.
const PLAYER_MASS: f32 = 85.0;

/// A player walking into props pushes them: a prop that may move is given at least the speed
/// the player means to go into it - all of it if it is no heavier than a player, less as it is
/// heavier. What the player stands on or bumps from above is not pushed.
fn push_props(lua: &Lua, mv: &MoveData, touched: &[(EntityHandle, Vector)]) {
    let (forward, right, _) = Angle::new(0.0, mv.angles.y, 0.0).vectors();
    let wish = forward * mv.forward + right * mv.side;
    let wish = if wish.length() > mv.max_speed && wish.length() > 0.0 { wish * (mv.max_speed / wish.length()) } else { wish };
    let mut pushed = Vec::new();
    for &(prop, normal) in touched {
        if normal.z.abs() > 0.7 || pushed.contains(&prop) {
            continue;
        }
        let Some(id) = crate::physics::body(lua, prop) else { continue };
        let into = -wish.dot(normal);
        if into <= 0.0 {
            continue;
        }
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        if !physics.motion(id) {
            continue;
        }
        pushed.push(prop);
        let mass = physics.mass(id).max(1.0);
        let speed = into * (PLAYER_MASS / mass).min(1.0);
        let v = physics.velocity(id);
        let direction = normal * -1.0;
        let along = v[0] * direction.x + v[1] * direction.y + v[2] * direction.z;
        if along < speed {
            let add = direction * (speed - along);
            physics.set_velocity(id, [v[0] + add.x, v[1] + add.y, v[2] + add.z]);
            physics.wake(id);
        }
    }
}

/// What a player on the ground stands on, and where it is now.
fn find_ground(lua: &Lua, handle: EntityHandle, moving: &gmod_movement::Player, settings: &Settings) {
    let ground = if moving.on_ground {
        let (mins, maxs) = moving.hull(settings);
        let down = Sweep {
            mins: vector(mins),
            maxs: vector(maxs),
            mask: gmod_bsp::mask::PLAYERSOLID,
            group: trace::group::PLAYER_MOVEMENT,
            ..Sweep::line(vector(moving.origin), vector(moving.origin) - Vector::new(0.0, 0.0, 2.0), Filter::Entities(vec![handle]))
        };
        let hit = trace::trace(lua, &down);
        if hit.fraction < 1.0 { hit.entity } else { EntityHandle::WORLD }
    } else {
        EntityHandle::NULL
    };
    let at = if ground.index.is_some_and(|i| i != 0) { host(lua).entities.transform(ground) } else { None };
    with_player(lua, handle, |_, p| (p.ground, p.ground_at) = (ground, at));
}

/// Where a player standing on something that moved is taken with it: as far as it moved and
/// turned under their feet since last tick, if nothing is in the way.
fn carried(lua: &Lua, handle: EntityHandle, origin: Vector, moving: &gmod_movement::Player, settings: &Settings) -> Vector {
    let Some((ground, Some((was, was_turned)))) = state(lua, handle, |_, p| (p.ground, p.ground_at)) else { return origin };
    let Some((now, turned)) = host(lua).entities.transform(ground) else { return origin };
    if now == was && turned == was_turned {
        return origin;
    }
    // where the feet stood on it, as it stands now
    let on = entity::unrotated(origin - was, was_turned);
    let to = now + on.rotated(turned);
    let (mins, maxs) = moving.hull(settings);
    let sweep = Sweep {
        mins: vector(mins),
        maxs: vector(maxs),
        mask: gmod_bsp::mask::PLAYERSOLID,
        group: trace::group::PLAYER_MOVEMENT,
        ..Sweep::line(origin, to, Filter::Entities(vec![handle, ground]))
    };
    let hit = trace::trace(lua, &sweep);
    let carried = if hit.start_solid { origin } else { hit.end };
    with_player(lua, handle, |_, p| p.ground_at = Some((now, turned)));
    carried
}

/// The end of a tick's movement: the player takes what the `CMoveData` says.
fn finish_move(lua: &Lua, handle: EntityHandle, mv: &MoveData) {
    entity::set_world_pos(lua, handle, mv.origin);
    entity::set_world_angles(lua, handle, Angle::new(0.0, mv.angles.y, 0.0));
    with_player(lua, handle, |ent, p| {
        ent.velocity = mv.velocity;
        p.eye_angles = mv.angles;
        let set = |flags: &mut i64, flag: i64, on: bool| if on { *flags |= flag } else { *flags &= !flag };
        set(&mut ent.flags, fl::ONGROUND, p.moving.on_ground && ent.move_type == MOVETYPE_WALK);
        set(&mut ent.flags, fl::DUCKING, p.moving.ducked);
        set(&mut ent.flags, fl::ANIMDUCKING, p.moving.ducked);
        set(&mut ent.flags, fl::INWATER, p.moving.water_level > 0);
        (ent.mins, ent.maxs) = if p.moving.ducked { p.duck_hull } else { p.hull };
    });
}

/// Footsteps, as Source makes them (`UpdateStepSound`): on the ground or a ladder and moving,
/// one every `PlayerStepSoundTime` milliseconds (the gamemode's hook; 400 walking, 300
/// running, 100 more ducked), left and right in turn, the sound of the surface underfoot -
/// or of the water waded in - as loud as the pace. `PlayerFootstep` returning true keeps one
/// quiet.
fn footsteps(lua: &Lua, handle: EntityHandle, ply: &AnyUserData) {
    const TICK_MS: f32 = (crate::timer::TICK_INTERVAL * 1000.0) as f32;
    let Some((moving, ducked_speed, origin, wait)) = with_player(lua, handle, |ent, p| {
        p.step_time = (p.step_time - TICK_MS).max(0.0);
        let walking = ent.move_type == MOVETYPE_WALK;
        (walking.then_some(p.moving), p.moving.ducked, ent.local_pos, p.step_time)
    }) else {
        return;
    };
    let Some(moving) = moving else { return };
    let ladder = moving.ladder.is_some();
    if wait > 0.0 || !(moving.on_ground || ladder) {
        return;
    }
    let [vx, vy, vz] = moving.velocity;
    let speed = if ladder { (vx * vx + vy * vy + vz * vz).sqrt() } else { (vx * vx + vy * vy).sqrt() };
    // the engine plays a step at any pace once the last one's time is up; what drifts is not
    // a pace
    let run = if ducked_speed || ladder { 80.0 } else { 220.0 };
    if speed < 10.0 {
        return;
    }
    let walking = speed < run;
    let kind = match moving.water_level {
        _ if ladder => 1,
        2 => 2,
        1 => 3,
        _ => 0,
    };
    let default_time = if walking { 400.0 } else { 300.0 };
    let time = match hook(lua, "PlayerStepSoundTime", [Value::UserData(ply.clone()), Value::Integer(kind), Value::Boolean(walking)]).front()
    {
        Some(Value::Number(ms)) => *ms as f32,
        Some(Value::Integer(ms)) => *ms as f32,
        _ => default_time,
    } + if ducked_speed { 100.0 } else { 0.0 };
    // the surface: the water waded in, else what is underfoot
    let surface = match moving.water_level {
        2.. => crate::surfaceprops::named(lua, "wade").1,
        1 => crate::surfaceprops::named(lua, "water").1,
        _ => {
            let down = Vector::new(origin.x, origin.y, origin.z - 64.0);
            let up = Vector::new(origin.x, origin.y, origin.z + 1.0);
            let hit = trace::trace(lua, &Sweep::line(up, down, Filter::Entities(vec![handle])));
            if hit.entity == EntityHandle::WORLD && !hit.texture.starts_with("**") {
                crate::surfaceprops::of_material(lua, &hit.texture).1
            } else {
                let body = crate::physics::material_of(lua, hit.entity).unwrap_or_else(|| "default".to_string());
                crate::surfaceprops::named(lua, &body).1
            }
        }
    };
    let right = with_player(lua, handle, |_, p| {
        p.step_time = time;
        p.step_right = !p.step_right;
        !p.step_right
    })
    .unwrap_or(false);
    let sound = if right { &surface.step_right } else { &surface.step_left };
    let mut volume = if walking { 0.2 } else { 0.5 };
    if ducked_speed {
        volume *= 0.65;
    }
    let pos = entity::vector(lua, origin).unwrap_or(Value::Nil);
    let quiet = hook(
        lua,
        "PlayerFootstep",
        [
            Value::UserData(ply.clone()),
            pos,
            Value::Integer(i64::from(right)),
            Value::String(match lua.create_string(sound) {
                Ok(s) => s,
                Err(_) => return,
            }),
            Value::Number(f64::from(volume)),
            Value::Nil,
        ],
    );
    if !truthy(&quiet) {
        // a script's volume is the loudest a step is; walking is quieter
        crate::sound::emit_scaled(lua, sound, handle, volume * 2.0);
    }
}

fn set_flashlight(lua: &Lua, handle: EntityHandle, on: bool) {
    host(lua).entities.with_mut(handle, |ent| if on { ent.effects |= EF_DIMLIGHT } else { ent.effects &= !EF_DIMLIGHT });
}

// --- what the program shows, and what a player types ---

/// What the program shows of a player: where they see from, how they are, what they hold.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerView {
    /// Where the player sees from after the last tick, and when that tick began: the picture
    /// goes between the two ([`crate::Runtime::tick_fraction`]).
    pub eye: Vector,
    pub previous_eye: Vector,
    pub angles: Angle,
    pub fov: f32,
    pub alive: bool,
    pub health: i64,
    pub armor: i64,
    pub weapon: Option<WeaponView>,
    /// The physics gun holds something: the mouse wheel takes it farther or nearer rather
    /// than choosing a weapon, and the mouse turns it while E is down.
    pub holding: bool,
    /// The flashlight is on (`EF_DIMLIGHT`).
    pub flashlight: bool,
    /// How many times the game turned the player itself ([`PlayerState::turned`]).
    pub turned: u32,
}

/// The weapon in a player's hands.
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponView {
    pub class: String,
    /// `PrintName`, often a `#` phrase of the game's language files.
    pub name: String,
    /// Rounds in the primary clip, -1 without one; rounds carried for it.
    pub clip: i64,
    pub ammo: i64,
}

pub(crate) fn view(lua: &Lua, handle: EntityHandle) -> Option<PlayerView> {
    let (eye, _) = eye(lua, handle)?;
    let (previous_eye, angles, fov, alive, armor, active, turned) =
        state(lua, handle, |_, p| (p.previous_eye.unwrap_or(eye), p.eye_angles, p.fov, p.alive, p.armor, p.active_weapon, p.turned))?;
    let health = host(lua).entities.get(handle).map_or(0, |ent| ent.health);
    let weapon = host(lua).entities.get(active).filter(|ent| !ent.removed).map(|ent| {
        let data = weapon::data(lua, active).unwrap_or_default();
        let clip = ent.weapon.as_ref().map_or(-1, |w| w.clip[0]);
        let ammo = ammo_count(lua, handle, weapon::ammo_id(lua, &data.ammo[0]));
        WeaponView { class: ent.class.clone(), name: data.print_name, clip, ammo }
    });
    let held = state(lua, handle, |_, p| p.physgun).filter(|&h| host(lua).entities.is_valid(h));
    let holding = crate::physgun::holding(lua, handle).or(held).is_some();
    let flashlight = host(lua).entities.get(handle).is_some_and(|ent| ent.effects & EF_DIMLIGHT != 0);
    Some(PlayerView { eye, previous_eye, angles, fov, alive, health, armor, weapon, holding, flashlight, turned })
}

/// The engine's commands a player types into the console: `noclip`, `kill` and `explode`,
/// `give <class>`, and choosing a weapon - `use <class>`, `lastinv`, `invnext`, `invprev`,
/// `slot1` to `slot6` (what the weapon selection of the client's HUD would send) - and, with
/// `sv_cheats 1`, `ent_fire <target> <input> [param] [delay]`. Whether `words` was one of
/// them, which then ran.
pub(crate) fn command(lua: &Lua, handle: EntityHandle, words: &[String]) -> bool {
    let Some((name, args)) = words.split_first() else { return false };
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "noclip" => toggle_noclip(lua, handle),
        "kill" | "explode" => suicide(lua, handle, name == "explode"),
        "give" => {
            if let Some(class) = args.first()
                && let Err(e) = give(lua, handle, class, false)
            {
                host(lua).report(crate::ScriptError::from_lua(&e));
            }
        }
        "use" => {
            if let Some(item) = args.first().and_then(|class| weapon_of(lua, handle, class)) {
                switch_weapon(lua, handle, item);
            }
        }
        "lastinv" => {
            let previous = state(lua, handle, |_, p| Some(p.previous_weapon).filter(|w| p.weapons.contains(w)));
            if let Some(Some(item)) = previous {
                switch_weapon(lua, handle, item);
            }
        }
        "ent_fire" => ent_fire(lua, handle, args),
        "invnext" => select_weapon(lua, handle, 1, None),
        "invprev" => select_weapon(lua, handle, -1, None),
        _ => match name.strip_prefix("slot").and_then(|n| n.parse::<i64>().ok()).filter(|n| (1..=6).contains(n)) {
            Some(slot) => select_weapon(lua, handle, 1, Some(slot - 1)),
            None => return set_user_info(lua, handle, &name, args),
        },
    }
    true
}

/// `ent_fire <target> <input> [param] [delay]`, a cheat: the input goes to what the target
/// names as an output's would (`!self` and `!activator` being the player, `!picker` what they
/// look at), the player its activator.
fn ent_fire(lua: &Lua, handle: EntityHandle, args: &[String]) {
    if convar::value(lua, "sv_cheats").is_none_or(|v| convar::atof(&v) == 0.0) {
        host(lua).print("Can't use cheat command ent_fire in multiplayer, unless the server has sv_cheats set to 1.\n");
        return;
    }
    let (Some(target), Some(input)) = (args.first(), args.get(1)) else { return };
    let param = args.get(2).map_or("", String::as_str);
    let delay = args.get(3).map_or(0.0, |d| convar::atof(d));
    if target.eq_ignore_ascii_case("!picker") {
        let Some((eye, aim)) = eye(lua, handle) else { return };
        let sweep = Sweep { mask: gmod_bsp::mask::SHOT, ..Sweep::line(eye, eye + aim * 8192.0, Filter::Entities(vec![handle])) };
        let picked = trace::trace(lua, &sweep).entity;
        if picked.index.is_some_and(|i| i != 0) {
            crate::inputs::fire(lua, picked, input, param, delay, handle, handle);
        }
        return;
    }
    crate::inputs::fire_named(lua, target, input, param, delay, handle, handle, handle);
}

/// Runs a line of console commands as typed by a player: the engine's commands for players,
/// their user info, ConVars and the commands of `concommand.Add`.
pub(crate) fn console(lua: &Lua, handle: EntityHandle, line: &str) {
    for words in convar::split_commands(line) {
        if !command(lua, handle, &words) {
            convar::execute_as(lua, &words, handle);
        }
    }
}

/// A player's user info, as their client would send it: none when there is no such setting.
pub(crate) fn user_info(lua: &Lua, handle: EntityHandle, name: &str) -> Option<String> {
    state(lua, handle, |_, p| p.user_info(name)).flatten().or_else(|| tool_setting(lua, name))
}

/// The setting of one of sandbox's tools that a client makes of `TOOL.ClientConVar`, as
/// `weld_forcelimit`: its default.
fn tool_setting(lua: &Lua, name: &str) -> Option<String> {
    let stored: Table = lua.globals().get::<Table>("weapons").ok()?.get::<mlua::Function>("GetStored").ok()?.call("gmod_tool").ok()?;
    let tools: Table = stored.get("Tool").ok()?;
    for (mode, tool) in tools.pairs::<String, Value>().flatten() {
        let Value::Table(tool) = tool else { continue };
        let setting =
            name.get(..mode.len()).filter(|start| start.eq_ignore_ascii_case(&mode)).and_then(|_| name[mode.len()..].strip_prefix('_'));
        let Some(setting) = setting else { continue };
        let Ok(Some(convars)) = tool.get::<Option<Table>>("ClientConVar") else { continue };
        for (key, value) in convars.pairs::<String, Value>().flatten() {
            if key.eq_ignore_ascii_case(setting) {
                return match value {
                    Value::String(s) => Some(s.to_string_lossy()),
                    Value::Integer(n) => Some(n.to_string()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Boolean(b) => Some(if b { "1" } else { "0" }.to_string()),
                    _ => None,
                };
            }
        }
    }
    None
}

/// `name value` typed by a player, when `name` is in their user info: it is set, as a client's
/// ConVar would be; `name` alone says what it is. Whether `name` was such.
fn set_user_info(lua: &Lua, handle: EntityHandle, name: &str, args: &[String]) -> bool {
    let Some(now) = user_info(lua, handle, name) else { return false };
    if args.is_empty() {
        host(lua).print(&format!("\"{name}\" = \"{now}\"\n"));
        return true;
    }
    store_user_info(lua, handle, name, &args.join(" "));
    true
}

/// A value of a player's user info, as their client says it.
pub(crate) fn store_user_info(lua: &Lua, handle: EntityHandle, name: &str, value: &str) {
    with_player(lua, handle, |_, p| {
        let own = &mut p.info.user_info;
        own.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        own.push((name.to_string(), value.to_string()));
    });
}

/// `noclip`: flying through walls and back, when `PlayerNoClip` allows it.
fn toggle_noclip(lua: &Lua, handle: EntityHandle) {
    let Some(move_type) = host(lua).entities.get(handle).filter(|ent| ent.player.is_some()).map(|ent| ent.move_type) else { return };
    let on = move_type != MOVETYPE_NOCLIP;
    if truthy(&hook(lua, "PlayerNoClip", [ent_value(lua, handle), Value::Boolean(on)])) {
        host(lua).entities.with_mut(handle, |ent| ent.move_type = if on { MOVETYPE_NOCLIP } else { MOVETYPE_WALK });
    }
}

/// `kill` and `explode`: the player dies by their own hand, unless `CanPlayerSuicide` says no.
fn suicide(lua: &Lua, handle: EntityHandle, blast: bool) {
    if state(lua, handle, |_, p| p.alive) != Some(true) {
        return;
    }
    if matches!(hook(lua, "CanPlayerSuicide", [ent_value(lua, handle)]).front(), Some(Value::Boolean(false))) {
        return;
    }
    let health = host(lua).entities.get(handle).map_or(0, |ent| ent.health);
    let kind = if blast { damage::dmg::BLAST } else { damage::dmg::GENERIC };
    kill(lua, handle, Death { info: DamageInfo::new(health.max(1) as f32, kind, handle, handle), userdata: None, silent: false });
}

/// The next weapon (`step` 1) or the one before (-1), in the order of the slots and the
/// positions in them; only those of `slot` (0 to 5) when it is given.
fn select_weapon(lua: &Lua, handle: EntityHandle, step: isize, slot: Option<i64>) {
    let Some((weapons, active)) = state(lua, handle, |_, p| (p.weapons.clone(), p.active_weapon)) else { return };
    let mut order: Vec<((i64, i64, u32), EntityHandle)> = weapons
        .into_iter()
        .filter_map(|w| {
            let data = weapon::data(lua, w)?;
            slot.is_none_or(|s| s == data.slot).then_some(((data.slot, data.slot_pos, w.index?), w))
        })
        .collect();
    order.sort_by_key(|(key, _)| *key);
    let list: Vec<EntityHandle> = order.into_iter().map(|(_, w)| w).collect();
    let Some(&first) = list.first() else { return };
    let next = match list.iter().position(|&w| w == active) {
        Some(i) => list[(i as isize + step).rem_euclid(list.len() as isize) as usize],
        None if step < 0 => list[list.len() - 1],
        None => first,
    };
    if next != active {
        switch_weapon(lua, handle, next);
    }
}

// --- weapons and ammo ---

/// Rounds of an ammo type a player carries (0 for anything that is not a player).
pub(crate) fn ammo_count(lua: &Lua, handle: EntityHandle, id: i64) -> i64 {
    state(lua, handle, |_, p| p.ammo.get(&id).copied().unwrap_or(0)).unwrap_or(0)
}

/// Sets the rounds of an ammo type, and tells `PlayerAmmoChanged`.
fn set_ammo(lua: &Lua, handle: EntityHandle, id: i64, count: i64) {
    if id < 1 {
        return;
    }
    let Some(old) = with_player(lua, handle, |_, p| {
        let old = p.ammo.get(&id).copied().unwrap_or(0);
        p.ammo.insert(id, count.max(0));
        old
    }) else {
        return;
    };
    if old != count.max(0) {
        hook(lua, "PlayerAmmoChanged", [ent_value(lua, handle), Value::Integer(id), Value::Integer(old), Value::Integer(count.max(0))]);
    }
}

/// Gives rounds, up to what the player may carry; how many were taken.
pub(crate) fn give_ammo(lua: &Lua, handle: EntityHandle, id: i64, amount: i64) -> i64 {
    if id < 1 || amount <= 0 || !is_player(lua, handle) {
        return 0;
    }
    let max = weapon::ammo_max(lua, id);
    if max < 0 {
        return 0;
    }
    let old = ammo_count(lua, handle, id);
    let new = (old + amount).min(max);
    set_ammo(lua, handle, id, new);
    new - old
}

pub(crate) fn remove_ammo(lua: &Lua, handle: EntityHandle, id: i64, amount: i64) {
    let old = ammo_count(lua, handle, id);
    if amount > 0 && old > 0 {
        set_ammo(lua, handle, id, (old - amount).max(0));
    }
}

/// The player's weapon of a class.
fn weapon_of(lua: &Lua, handle: EntityHandle, class: &str) -> Option<EntityHandle> {
    let weapons = state(lua, handle, |_, p| p.weapons.clone())?;
    let entities = &host(lua).entities;
    weapons.into_iter().find(|&w| entities.get(w).is_some_and(|ent| !ent.removed && ent.class.eq_ignore_ascii_case(class)))
}

/// `Give`: a new weapon of a class in the player's hands, or `NULL` when they have one already,
/// the class is unknown or `PlayerCanPickupWeapon` says no. Something that is not a weapon is
/// made at the player's feet.
pub(crate) fn give(lua: &Lua, handle: EntityHandle, class: &str, no_ammo: bool) -> mlua::Result<EntityHandle> {
    if weapon_of(lua, handle, class).is_some() {
        return Ok(EntityHandle::NULL);
    }
    let Some(item) = crate::ents::create(lua, class, None)? else { return Ok(EntityHandle::NULL) };
    let (pos, _) = host(lua).entities.transform(handle).unwrap_or_default();
    entity::set_world_pos(lua, item, pos);
    crate::ents::spawn(lua, item);
    if item.kind != Kind::Weapon || !host(lua).entities.is_valid(item) {
        return Ok(item);
    }
    let allowed = hook(lua, "PlayerCanPickupWeapon", [ent_value(lua, handle), ent_value(lua, item)]);
    if matches!(allowed.front(), Some(Value::Boolean(false))) {
        crate::ents::remove(lua, item);
        return Ok(EntityHandle::NULL);
    }
    equip(lua, handle, item, no_ammo);
    Ok(item)
}

/// A player touches a weapon on the ground (`BumpWeapon`). Having one of its class, they take
/// the ammo in it - what is in its clips, or what it would give without clips - and it goes
/// if they took any; else they pick it up, if `PlayerCanPickupWeapon` lets them.
pub(crate) fn bump_weapon(lua: &Lua, handle: EntityHandle, item: EntityHandle) {
    let Some(class) = host(lua).entities.get(item).map(|ent| ent.class.clone()) else { return };
    let data = weapon::data(lua, item).unwrap_or_default();
    if weapon_of(lua, handle, &class).is_some() {
        let clips = host(lua).entities.get(item).and_then(|ent| ent.weapon.as_ref().map(|w| w.clip)).unwrap_or([-1, -1]);
        let mut took = 0;
        for (i, clip) in clips.into_iter().enumerate() {
            let amount = if clip >= 0 { clip } else { data.default_clip[i].max(0) };
            took += give_ammo(lua, handle, weapon::ammo_id(lua, &data.ammo[i]), amount);
        }
        if took > 0 {
            crate::ents::remove(lua, item);
        }
        return;
    }
    let allowed = hook(lua, "PlayerCanPickupWeapon", [ent_value(lua, handle), ent_value(lua, item)]);
    if matches!(allowed.front(), Some(Value::Boolean(false))) || !host(lua).entities.is_valid(item) {
        return;
    }
    crate::physics::destroy(lua, item);
    equip(lua, handle, item, false);
}

/// How far the use key reaches, from the eyes.
const USE_REACH: f32 = 80.0;
pub(crate) const IN_USE: u32 = 32;
const IN_ATTACK: u32 = 1;
const IN_ATTACK2: u32 = 2048;
/// `SetUseType`'s kinds, and the kinds of use `ENT:Use` is given.
const CONTINUOUS_USE: i64 = 0;
const ONOFF_USE: i64 = 1;
const USE_OFF: i64 = 0;
const USE_ON: i64 = 1;

/// What a player would use: the entity before their eyes within reach - by a ray, or else a
/// small box for the thin and the small - as `FindUseEntity` may change it.
fn find_use_entity(lua: &Lua, handle: EntityHandle) -> EntityHandle {
    let Some((eye, aim)) = eye(lua, handle) else { return EntityHandle::NULL };
    let end = eye + aim * USE_REACH;
    let mut found = EntityHandle::NULL;
    for size in [0.0, 8.0] {
        let reach = Vector::new(size, size, size);
        let sweep =
            Sweep { mins: reach * -1.0, maxs: reach, mask: gmod_bsp::mask::SOLID, ..Sweep::line(eye, end, Filter::Entities(vec![handle])) };
        let hit = trace::trace(lua, &sweep).entity;
        if hit.index.is_some_and(|i| i != 0) {
            found = hit;
            break;
        }
    }
    let chosen = hook(lua, "FindUseEntity", [ent_value(lua, handle), ent_value(lua, found)]);
    match chosen.front() {
        Some(value @ Value::UserData(_)) => entity::handle_of(Some(value)).unwrap_or(found),
        _ => found,
    }
}

/// The use key (`+use`): pressed, the player uses what is before their eyes if `PlayerUse`
/// lets them - a scripted entity's `ENT:Use`, the engine's `Use` input (doors, buttons) -
/// and hears the denial when there is nothing. A scripted entity used continuously
/// (`CONTINUOUS_USE`, the default) is used every tick while the key is held and it is still
/// there; one used on and off (`ONOFF_USE`) is used off when the key is let go.
fn use_key(lua: &Lua, handle: EntityHandle, buttons: u32, pressed: u32) {
    if crate::physgun::holding(lua, handle).is_some() {
        return;
    }
    let using = state(lua, handle, |_, p| p.using).unwrap_or(EntityHandle::NULL);
    let use_type = |e: EntityHandle| host(lua).entities.get(e).map_or(-1, |ent| ent.use_type);
    if buttons & IN_USE == 0 {
        if host(lua).entities.is_valid(using) && use_type(using) == ONOFF_USE {
            crate::inputs::use_entity(lua, using, handle, handle, USE_OFF, 0.0);
        }
        with_player(lua, handle, |_, p| p.using = EntityHandle::NULL);
        return;
    }
    if pressed & IN_USE == 0 {
        // held: what is used continuously, while it is still before the eyes
        if host(lua).entities.is_valid(using) && use_type(using) == CONTINUOUS_USE && find_use_entity(lua, handle) == using {
            crate::inputs::use_entity(lua, using, handle, handle, USE_ON, 1.0);
        }
        return;
    }
    // carrying something, the use key lets it go
    if crate::pickup::carrying(lua, handle).is_some() {
        crate::pickup::drop(lua, handle, false);
        return;
    }
    let target = find_use_entity(lua, handle);
    if !host(lua).entities.is_valid(target) {
        crate::sound::emit_scaled(lua, "HL2Player.UseDeny", handle, 1.0);
        return;
    }
    let allowed = hook(lua, "PlayerUse", [ent_value(lua, handle), ent_value(lua, target)]);
    if matches!(allowed.front(), Some(Value::Boolean(false))) {
        return;
    }
    // a vehicle is sat in
    if crate::vehicle::is_vehicle(lua, target) {
        if !crate::vehicle::enter(lua, handle, target, false) {
            crate::sound::emit_scaled(lua, "HL2Player.UseDeny", handle, 1.0);
        }
        return;
    }
    let scripted = host(lua).entities.get(target).is_some_and(|ent| ent.scripted);
    // a prop is picked up; so is a scripted entity of physics that does nothing when used
    let prop =
        host(lua).entities.get(target).is_some_and(|ent| !ent.scripted && ent.class.to_ascii_lowercase().starts_with("prop_physics"));
    if (prop || scripted && !has_own_use(lua, target)) && crate::pickup::can_carry(lua, target) {
        if !crate::pickup::pick_up(lua, handle, target, false) {
            crate::sound::emit_scaled(lua, "HL2Player.UseDeny", handle, 1.0);
        }
        return;
    }
    if (scripted || crate::items::is_charger(lua, target)) && matches!(use_type(target), CONTINUOUS_USE | ONOFF_USE) {
        with_player(lua, handle, |_, p| p.using = target);
    }
    if !crate::inputs::use_entity(lua, target, handle, handle, USE_ON, 1.0) {
        crate::sound::emit_scaled(lua, "HL2Player.UseDeny", handle, 1.0);
    }
}

/// Whether a scripted entity has an `ENT:Use` of its own.
fn has_own_use(lua: &Lua, handle: EntityHandle) -> bool {
    let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { return false };
    entity::table_of(&ud).ok().flatten().is_some_and(|t| matches!(t.get::<Value>("Use"), Ok(Value::Function(_))))
}

/// Turns where a player looks (`SetEyeAngles`); their body turns with the yaw.
pub(crate) fn set_eye_angles(lua: &Lua, handle: EntityHandle, angles: Angle) {
    with_player(lua, handle, |_, p| {
        p.eye_angles = angles;
        p.input.view_angles = angles;
        p.turned = p.turned.wrapping_add(1);
    });
    entity::set_world_angles(lua, handle, Angle::new(0.0, angles.y, 0.0));
}

/// A weapon goes into a player's hands (`Weapon_Equip`): it follows them, its ammo beyond a
/// full clip goes to the player (none with `no_ammo`), the SWEP's `Equip` and the
/// `WeaponEquip` hook run, and the player takes it up if it is better than what they hold.
pub(crate) fn equip(lua: &Lua, handle: EntityHandle, item: EntityHandle, no_ammo: bool) {
    let data = weapon::data(lua, item).unwrap_or_default();
    entity::set_parent(lua, item, handle);
    host(lua).entities.with_mut(item, |ent| {
        ent.owner = handle;
        ent.local_pos = Vector::default();
        ent.local_angles = Angle::default();
        ent.solid = 0;
        ent.move_type = MOVETYPE_NONE;
        ent.velocity = Vector::default();
        ent.effects |= EF_BONEMERGE | EF_NODRAW;
    });
    with_player(lua, handle, |_, p| p.weapons.push(item));
    for i in 0..2 {
        let id = weapon::ammo_id(lua, &data.ammo[i]);
        let extra = if data.clip_size[i] < 0 {
            data.default_clip[i]
        } else if data.default_clip[i] > data.clip_size[i] {
            host(lua).entities.with_mut(item, |ent| {
                if let Some(w) = ent.weapon.as_mut() {
                    w.clip[i] = data.clip_size[i];
                }
            });
            data.default_clip[i] - data.clip_size[i]
        } else {
            0
        };
        if !no_ammo {
            give_ammo(lua, handle, id, extra);
        }
    }
    let owner = ent_value(lua, handle);
    weapon::call(lua, item, "Equip", MultiValue::from_iter([owner.clone()]));
    weapon::call(lua, item, "OwnerChanged", MultiValue::new());
    hook(lua, "WeaponEquip", [ent_value(lua, item), owner]);
    let active = state(lua, handle, |_, p| p.active_weapon).unwrap_or(EntityHandle::NULL);
    let better = match weapon::data(lua, active).filter(|_| host(lua).entities.is_valid(active)) {
        None => true,
        Some(current) => data.weight > current.weight && current.auto_switch_from && data.auto_switch_to,
    };
    if better && host(lua).entities.is_valid(item) {
        switch_weapon(lua, handle, item);
    }
}

/// Takes up one of the player's weapons (`SelectWeapon`): `PlayerSwitchWeapon` may stop it
/// (returning true), and so may the old weapon's `Holster` (returning false); then the new
/// one's `Deploy` runs.
pub(crate) fn switch_weapon(lua: &Lua, handle: EntityHandle, item: EntityHandle) -> bool {
    let Some((old, has)) = state(lua, handle, |_, p| (p.active_weapon, p.weapons.contains(&item))) else { return false };
    if !has || !host(lua).entities.is_valid(item) {
        return false;
    }
    if old == item {
        return true;
    }
    let old_valid = host(lua).entities.is_valid(old);
    let blocked = hook(lua, "PlayerSwitchWeapon", [ent_value(lua, handle), ent_value(lua, old), ent_value(lua, item)]);
    if truthy(&blocked) {
        return false;
    }
    if old_valid {
        let holster = weapon::call(lua, old, "Holster", MultiValue::from_iter([ent_value(lua, item)]));
        if matches!(holster.as_ref().and_then(|r| r.front()), Some(Value::Boolean(false))) {
            return false;
        }
        host(lua).entities.with_mut(old, |ent| ent.effects |= EF_NODRAW);
    }
    with_player(lua, handle, |_, p| {
        p.previous_weapon = if old_valid { old } else { EntityHandle::NULL };
        p.active_weapon = item;
    });
    host(lua).entities.with_mut(item, |ent| ent.effects &= !EF_NODRAW);
    // the engine puts the weapon's view model on and plays drawing it; the weapon's own
    // `Deploy` comes after, and may play something else
    crate::anim::deploy(lua, handle, item);
    weapon::call(lua, item, "Deploy", MultiValue::new());
    true
}

/// A weapon leaves the player's hands for the world (`DropWeapon`).
pub(crate) fn drop_weapon(lua: &Lua, handle: EntityHandle, item: EntityHandle, velocity: Option<Vector>) {
    let had = with_player(lua, handle, |_, p| {
        let had = p.weapons.contains(&item);
        p.weapons.retain(|&w| w != item);
        if p.active_weapon == item {
            p.active_weapon = EntityHandle::NULL;
        }
        had
    });
    if had != Some(true) {
        return;
    }
    entity::set_parent(lua, item, EntityHandle::NULL);
    host(lua).entities.with_mut(item, |ent| {
        ent.owner = EntityHandle::NULL;
        ent.solid = SOLID_BBOX;
        ent.effects &= !(EF_BONEMERGE | EF_NODRAW);
        ent.velocity = velocity.unwrap_or_default();
    });
    weapon::call(lua, item, "OnDrop", MultiValue::new());
    weapon::call(lua, item, "OwnerChanged", MultiValue::new());
    hook(lua, "PlayerDroppedWeapon", [ent_value(lua, handle), ent_value(lua, item)]);
}

/// Takes a weapon from a player and removes it.
fn strip_weapon(lua: &Lua, handle: EntityHandle, item: EntityHandle) {
    with_player(lua, handle, |_, p| {
        p.weapons.retain(|&w| w != item);
        if p.active_weapon == item {
            p.active_weapon = EntityHandle::NULL;
        }
    });
    crate::ents::remove(lua, item);
}

fn strip_weapons(lua: &Lua, handle: EntityHandle) {
    let weapons = state(lua, handle, |_, p| p.weapons.clone()).unwrap_or_default();
    for item in weapons {
        strip_weapon(lua, handle, item);
    }
    with_player(lua, handle, |_, p| p.previous_weapon = EntityHandle::NULL);
}

/// An entity is going: it is no longer anyone's weapon, hands or view entity.
pub(crate) fn forget(lua: &Lua, gone: EntityHandle) {
    for handle in all(lua) {
        with_player(lua, handle, |_, p| {
            p.weapons.retain(|&w| w != gone);
            for field in [&mut p.active_weapon, &mut p.previous_weapon, &mut p.hands, &mut p.view_entity, &mut p.ragdoll, &mut p.driving] {
                if *field == gone {
                    *field = EntityHandle::NULL;
                }
            }
        });
    }
}

/// The death ragdoll (`CreateRagdoll`): an `hl2mp_ragdoll` with the player's model, in place
/// of the last one - a ragdoll ([`crate::ragdoll`]) posed as the player was drawn, falling on
/// as the player moved. A model without the physics of one leaves the corpse unseen. In GMod
/// the corpse is the client's alone, so here it lies on the world and on what does not move,
/// but goes through props that move, which neither push it nor feel it.
fn create_ragdoll(lua: &Lua, handle: EntityHandle) -> mlua::Result<()> {
    let old = state(lua, handle, |_, p| p.ragdoll).unwrap_or(EntityHandle::NULL);
    if host(lua).entities.is_valid(old) {
        crate::ents::remove(lua, old);
    }
    let Some(ragdoll) = crate::ents::create_engine(lua, "hl2mp_ragdoll", Kind::Entity)? else { return Ok(()) };
    let (pos, _) = host(lua).entities.transform(handle).unwrap_or_default();
    let angles = state(lua, handle, |_, p| p.render_angles).unwrap_or_default();
    let (model, skin, color, anim, velocity) = host(lua)
        .entities
        .get(handle)
        .map(|ent| (ent.model.clone(), ent.skin, ent.color, ent.anim.clone(), ent.velocity))
        .unwrap_or_default();
    entity::set_model(lua, ragdoll, &model);
    entity::set_world_pos(lua, ragdoll, pos);
    entity::set_world_angles(lua, ragdoll, angles);
    host(lua).entities.with_mut(ragdoll, |ent| {
        ent.owner = handle;
        ent.skin = skin;
        ent.color = color;
        ent.anim = anim;
    });
    if crate::ragdoll::init(lua, ragdoll, velocity) {
        let bodies = crate::ragdoll::bodies(lua, ragdoll).unwrap_or_default();
        let host = host(lua);
        let mut physics = host.physics.physics.borrow_mut();
        for id in bodies {
            physics.set_aloof(id, true);
        }
    }
    with_player(lua, handle, |_, p| p.ragdoll = ragdoll);
    Ok(())
}

// --- Lua ---

/// The player in argument 1.
pub(crate) fn this(a: &Args) -> mlua::Result<EntityHandle> {
    let handle = entity::this(a, 1)?;
    if is_player(a.lua, handle) { Ok(handle) } else { Err(a.bad(1, "Player")) }
}

fn read<R>(a: &Args, f: impl FnOnce(&Ent, &PlayerState) -> R) -> mlua::Result<R> {
    let handle = this(a)?;
    Ok(state(a.lua, handle, f).expect("checked above"))
}

fn write(a: &Args, f: impl FnOnce(&mut Ent, &mut PlayerState)) -> mlua::Result<Value> {
    let handle = this(a)?;
    with_player(a.lua, handle, f);
    Ok(Value::Nil)
}

fn number(n: f32) -> mlua::Result<Value> {
    Ok(Value::Number(f64::from(n)))
}

fn integer(n: i64) -> mlua::Result<Value> {
    Ok(Value::Integer(n))
}

fn boolean(b: bool) -> mlua::Result<Value> {
    Ok(Value::Boolean(b))
}

fn string(lua: &Lua, text: &str) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(text)?))
}

fn nothing(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    Ok(Value::Nil)
}

fn no(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    Ok(Value::Boolean(false))
}

fn null(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    entity::push(a.lua, EntityHandle::NULL)
}

fn zero(a: &Args) -> mlua::Result<Value> {
    this(a)?;
    Ok(Value::Integer(0))
}

/// An entity argument that may be NULL or nil.
fn opt_entity(a: &Args, position: usize) -> mlua::Result<EntityHandle> {
    match a.get(position) {
        None => Ok(EntityHandle::NULL),
        Some(value) => entity::handle_of(Some(value)).ok_or_else(|| a.bad(position, "Entity")),
    }
}

fn opt_vector(a: &Args, position: usize) -> mlua::Result<Option<Vector>> {
    match a.get(position) {
        None => Ok(None),
        Some(_) => a.userdata::<Vector>(position, "Vector").map(Some),
    }
}

fn key_arg(a: &Args) -> mlua::Result<u32> {
    Ok(a.number(2)? as i64 as u32)
}

/// A weapon argument: an entity, or the class of one of the player's weapons.
fn weapon_arg(a: &Args, handle: EntityHandle, position: usize) -> mlua::Result<EntityHandle> {
    match a.get(position) {
        Some(Value::String(class)) => Ok(weapon_of(a.lua, handle, &class.to_string_lossy()).unwrap_or(EntityHandle::NULL)),
        _ => opt_entity(a, position),
    }
}

/// Prints what a player was sent to read: the player of a single-player game reads the
/// server's console.
/// A line for the consoles of players' clients.
fn print_to(a: &Args, players: Vec<EntityHandle>, text: &str) {
    for player in players {
        to_client(a.lua, player, ToClient::Print(text.to_string()));
    }
}

macro_rules! float_field {
    ($def:expr, $get:literal, $set:literal, $field:ident) => {
        $def($get, |a| number(read(a, |_, p| p.$field)?))?;
        $def($set, |a| {
            let n = a.number(2)? as f32;
            write(a, |_, p| p.$field = n)
        })?;
    };
}

macro_rules! bool_field {
    ($def:expr, $get:literal, $set:literal, $field:ident) => {
        $def($get, |a| boolean(read(a, |_, p| p.$field)?))?;
        $def($set, |a| {
            let b = a.boolean(2);
            write(a, |_, p| p.$field = b)
        })?;
    };
}

macro_rules! entity_field {
    ($def:expr, $get:literal, $set:literal, $field:ident) => {
        $def($get, |a| {
            let handle = read(a, |_, p| p.$field)?;
            entity::push(a.lua, handle)
        })?;
        $def($set, |a| {
            let e = opt_entity(a, 2)?;
            write(a, |_, p| p.$field = e)
        })?;
    };
}

macro_rules! vector_field {
    ($def:expr, $get:literal, $set:literal, $($field:tt)+) => {
        $def($get, |a| entity::vector(a.lua, read(a, |_, p| p.$($field)+)?))?;
        $def($set, |a| {
            let v = a.userdata::<Vector>(2, "Vector")?;
            write(a, |_, p| p.$($field)+ = v)
        })?;
    };
}

macro_rules! angle_field {
    ($def:expr, $get:literal, $set:literal, $field:ident) => {
        $def($get, |a| entity::angle(a.lua, read(a, |_, p| p.$field)?))?;
        $def($set, |a| {
            let v = a.userdata::<Angle>(2, "Angle")?;
            write(a, |_, p| p.$field = v)
        })?;
    };
}

fn def_lib(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn list(lua: &Lua, handles: impl IntoIterator<Item = EntityHandle>) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    for handle in handles {
        t.raw_push(entity::push(lua, handle)?)?;
    }
    Ok(Value::Table(t))
}

pub(crate) fn install(lua: &Lua, realm: crate::Realm) -> mlua::Result<()> {
    let globals = lua.globals();
    def_lib(lua, &globals, "Player", |a| {
        let id = a.number(1)? as i64;
        let found = all(a.lua).into_iter().find(|&h| state(a.lua, h, |_, p| p.user_id == id).unwrap_or(false));
        entity::push(a.lua, found.unwrap_or(EntityHandle::NULL))
    })?;
    let library = lua.create_table()?;
    def_lib(lua, &library, "GetAll", |a| list(a.lua, all(a.lua)))?;
    def_lib(lua, &library, "GetHumans", |a| {
        let humans: Vec<_> = all(a.lua).into_iter().filter(|&h| state(a.lua, h, |_, p| !p.info.bot).unwrap_or(false)).collect();
        list(a.lua, humans)
    })?;
    def_lib(lua, &library, "GetBots", |a| {
        let bots: Vec<_> = all(a.lua).into_iter().filter(|&h| state(a.lua, h, |_, p| p.info.bot).unwrap_or(false)).collect();
        list(a.lua, bots)
    })?;
    def_lib(lua, &library, "GetCount", |a| Ok(Value::Integer(all(a.lua).len() as i64)))?;
    // a client joins at once: none is ever half way
    def_lib(lua, &library, "GetCountConnecting", |_| Ok(Value::Integer(0)))?;
    def_lib(lua, &library, "GetByID", |a| {
        let index = a.number(1)? as i64;
        let found = all(a.lua).into_iter().find(|h| h.index.map(i64::from) == Some(index));
        entity::push(a.lua, found.unwrap_or(EntityHandle::NULL))
    })?;
    if realm == crate::Realm::Server {
        def_lib(lua, &library, "CreateNextBot", |a| {
            let name = a.string(1)?;
            let joined = join(a.lua, PlayerInfo { name, bot: true, ..PlayerInfo::default() });
            entity::push(a.lua, joined.unwrap_or(EntityHandle::NULL))
        })?;
        // to every player: `HUD_PRINTTALK` into the chat (the base gamemode tells of a team
        // changed so), the rest to the console
        def_lib(lua, &globals, "PrintMessage", |a| {
            let text = a.string(2)?;
            if a.number(1)? as i64 == HUD_PRINTTALK {
                for player in all(a.lua) {
                    crate::chat::tell(a.lua, player, &text);
                }
            } else {
                print_to(a, all(a.lua), &text);
            }
            Ok(Value::Nil)
        })?;
        // to every player's console, as `PrintMessage(HUD_PRINTCONSOLE)` (the base gamemode
        // tells so of a player an NPC killed)
        def_lib(lua, &globals, "MsgAll", |a| {
            let text = crate::console::join(a.lua, &a.values, "")?;
            print_to(a, all(a.lua), &text);
            Ok(Value::Nil)
        })?;
        def_lib(lua, &globals, "BroadcastLua", |a| {
            let code = a.string(1)?;
            for player in all(a.lua) {
                to_client(a.lua, player, ToClient::Lua(code.clone()));
            }
            Ok(Value::Nil)
        })?;
    }
    globals.raw_set("player", library)?;

    let meta = crate::meta::meta_table(lua, "Player", crate::meta::type_id::ENTITY)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        meta.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    install_identity(&def)?;
    install_life(&def)?;
    install_movement(&def)?;
    install_weapons(&def)?;
    install_view(&def)?;
    install_rest(&def)?;
    let many = |name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>| -> mlua::Result<()> {
        meta.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    many("GetHull", |a| {
        let (mins, maxs) = read(a, |_, p| p.hull)?;
        Ok(MultiValue::from_iter([entity::vector(a.lua, mins)?, entity::vector(a.lua, maxs)?]))
    })?;
    many("GetHullDuck", |a| {
        let (mins, maxs) = read(a, |_, p| p.duck_hull)?;
        Ok(MultiValue::from_iter([entity::vector(a.lua, mins)?, entity::vector(a.lua, maxs)?]))
    })?;
    // sitting in vehicles
    crate::vehicle::install(lua)
}

/// Who the player is.
fn install_identity(def: &entity::Def) -> mlua::Result<()> {
    def("Nick", |a| {
        let name = read(a, |_, p| p.info.name.clone())?;
        string(a.lua, &name)
    })?;
    def("UserID", |a| integer(read(a, |_, p| p.user_id)?))?;
    def("SteamID", |a| {
        let id = read(a, |_, p| p.steam_id())?;
        string(a.lua, &id)
    })?;
    def("SteamID64", |a| {
        let id = read(a, |_, p| p.steam_id64())?;
        string(a.lua, &id.to_string())
    })?;
    def("OwnerSteamID64", |a| {
        let id = read(a, |_, p| p.steam_id64())?;
        string(a.lua, &id.to_string())
    })?;
    def("AccountID", |a| {
        let id = read(a, |_, p| p.steam_id64())?;
        integer(if id == 0 { 0 } else { i64::from(id as u32) })
    })?;
    def("UniqueID", |a| {
        let single = max_players(a.lua) == 1;
        integer(read(a, |_, p| p.unique_id(single))?.into())
    })?;
    def("UniqueIDTable", |a| {
        let single = max_players(a.lua) == 1;
        let (unique, key) = (read(a, |_, p| p.unique_id(single))?, a.string(2)?);
        let tables: Table = match a.lua.named_registry_value::<Option<Table>>("gmod-lua.unique-id-tables")? {
            Some(t) => t,
            None => {
                let t = a.lua.create_table()?;
                a.lua.set_named_registry_value("gmod-lua.unique-id-tables", &t)?;
                t
            }
        };
        let name = format!("{unique}{key}");
        match tables.raw_get::<Option<Table>>(name.as_str())? {
            Some(t) => Ok(Value::Table(t)),
            None => {
                let t = a.lua.create_table()?;
                tables.raw_set(name, &t)?;
                Ok(Value::Table(t))
            }
        }
    })?;
    def("IPAddress", |a| {
        let address = read(a, |_, p| p.info.address.clone())?;
        string(a.lua, &address)
    })?;
    def("IsBot", |a| boolean(read(a, |_, p| p.info.bot)?))?;
    def("IsConnected", |a| boolean(this(a).is_ok()))?;
    def("IsFullyAuthenticated", |a| boolean(read(a, |_, p| !p.info.bot && p.info.steam_id64.is_some())?))?;
    def("IsListenServerHost", |a| {
        let handle = this(a)?;
        boolean(is_host(a.lua, handle) && read(a, |_, p| !p.info.bot)?)
    })?;
    def("GetInfo", |a| {
        let name = a.string(2)?;
        let value = user_info(a.lua, this(a)?, &name);
        string(a.lua, value.as_deref().unwrap_or(""))
    })?;
    def("GetInfoNum", |a| {
        let (name, default) = (a.string(2)?, a.number(3)?);
        let value = user_info(a.lua, this(a)?, &name);
        Ok(Value::Number(value.and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(default)))
    })?;
    def("GetPlayerInfo", |a| {
        let handle = this(a)?;
        let (name, user_id, steam_id, account, bot) =
            read(a, |_, p| (p.info.name.clone(), p.user_id, p.steam_id(), p.steam_id64() as u32, p.info.bot))?;
        let _ = handle;
        let t = a.lua.create_table()?;
        t.raw_set("name", name)?;
        t.raw_set("userid", user_id)?;
        t.raw_set("guid", steam_id)?;
        t.raw_set("friendid", account)?;
        t.raw_set("friendname", "")?;
        t.raw_set("fakeplayer", bot)?;
        t.raw_set("ishltv", false)?;
        t.raw_set("isreplay", false)?;
        t.raw_set("customfiles", a.lua.create_table()?)?;
        Ok(Value::Table(t))
    })?;
    def("Team", |a| integer(read(a, |_, p| p.team)?))?;
    def("SetTeam", |a| {
        let team = a.number(2)? as i64;
        write(a, |_, p| p.team = team)
    })?;
    def("Frags", |a| integer(read(a, |_, p| p.frags)?))?;
    def("Deaths", |a| integer(read(a, |_, p| p.deaths)?))?;
    def("SetFrags", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.frags = n)
    })?;
    def("SetDeaths", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.deaths = n)
    })?;
    def("AddFrags", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.frags += n)
    })?;
    def("AddDeaths", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.deaths += n)
    })?;
    def("GetClassID", |a| integer(read(a, |_, p| p.class_id)?))?;
    def("SetClassID", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.class_id = n)
    })?;
    def("TimeConnected", |a| {
        let joined = read(a, |_, p| p.joined)?;
        Ok(Value::Number(host(a.lua).clock.cur_time.get() - joined))
    })?;
    def("Ping", zero)?;
    def("PacketLoss", zero)?;
    def("GetTimeoutSeconds", zero)?;
    def("IsTimingOut", no)?;
    def("IsSpeaking", no)?;
    def("IsTyping", no)?;
    def("IsPlayingTaunt", no)?;
    Ok(())
}

/// Life, death, health and armor.
fn install_life(def: &entity::Def) -> mlua::Result<()> {
    def("Alive", |a| boolean(read(a, |_, p| p.alive)?))?;
    def("Armor", |a| integer(read(a, |_, p| p.armor)?))?;
    def("SetArmor", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.armor = n)
    })?;
    def("GetMaxArmor", |a| integer(read(a, |_, p| p.max_armor)?))?;
    def("SetMaxArmor", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.max_armor = n)
    })?;
    def("Kill", |a| {
        let handle = this(a)?;
        let health = host(a.lua).entities.get(handle).map_or(0, |ent| ent.health);
        let info = DamageInfo::new(health.max(1) as f32, damage::dmg::GENERIC, handle, handle);
        kill(a.lua, handle, Death { info, userdata: None, silent: false });
        Ok(Value::Nil)
    })?;
    def("KillSilent", |a| {
        let handle = this(a)?;
        kill(a.lua, handle, Death { info: DamageInfo::new(0.0, damage::dmg::GENERIC, handle, handle), userdata: None, silent: true });
        Ok(Value::Nil)
    })?;
    def("CreateRagdoll", |a| {
        let handle = this(a)?;
        create_ragdoll(a.lua, handle)?;
        Ok(Value::Nil)
    })?;
    def("GetRagdollEntity", |a| {
        let ragdoll = read(a, |_, p| p.ragdoll)?;
        entity::push(a.lua, ragdoll)
    })?;
    def("LastHitGroup", |a| integer(read(a, |_, p| p.last_hit_group)?))?;
    def("SetLastHitGroup", |a| {
        let n = a.number(2)? as i64;
        write(a, |_, p| p.last_hit_group = n)
    })?;
    def("EquipSuit", |a| write(a, |_, p| p.suit = true))?;
    def("RemoveSuit", |a| write(a, |_, p| p.suit = false))?;
    def("IsSuitEquipped", |a| boolean(read(a, |_, p| p.suit)?))?;
    float_field!(def, "GetSuitPower", "SetSuitPower", suit_power);
    // `Spectate(mode)`: an observer flies through everything and is neither seen nor solid
    def("Spectate", |a| {
        let mode = a.number(2)? as i64;
        write(a, |ent, p| {
            p.observer_mode = mode;
            ent.move_type = MOVETYPE_OBSERVER;
            ent.solid_flags |= FSOLID_NOT_SOLID;
            ent.effects |= EF_NODRAW;
        })
    })?;
    def("UnSpectate", |a| {
        write(a, |ent, p| {
            if p.observer_mode != 0 || ent.move_type == MOVETYPE_OBSERVER {
                p.observer_mode = 0;
                p.observer_target = EntityHandle::NULL;
                ent.move_type = MOVETYPE_WALK;
                ent.solid_flags &= !FSOLID_NOT_SOLID;
                ent.effects &= !EF_NODRAW;
            }
        })
    })?;
    def("SpectateEntity", |a| {
        let target = opt_entity(a, 2)?;
        write(a, |_, p| p.observer_target = target)
    })?;
    def("GetObserverMode", |a| integer(read(a, |_, p| p.observer_mode)?))?;
    def("SetObserverMode", |a| {
        let mode = a.number(2)? as i64;
        write(a, |_, p| p.observer_mode = mode)
    })?;
    def("GetObserverTarget", |a| {
        let target = read(a, |_, p| p.observer_target)?;
        entity::push(a.lua, target)
    })?;
    // what `Freeze` and god mode are made of, together: no input, no damage
    def("Lock", |a| write(a, |ent, _| ent.flags |= fl::FROZEN | fl::GODMODE))?;
    def("UnLock", |a| write(a, |ent, _| ent.flags &= !(fl::FROZEN | fl::GODMODE)))?;
    def("SetNoTarget", |a| {
        let on = a.boolean(2);
        write(a, |ent, _| if on { ent.flags |= fl::NOTARGET } else { ent.flags &= !fl::NOTARGET })
    })?;
    // leaving is what kicking does; a ban would only matter to the next connection
    def("Kick", |a| {
        let handle = this(a)?;
        leave(a.lua, handle);
        Ok(Value::Nil)
    })?;
    def("Ban", |a| {
        let handle = this(a)?;
        if a.boolean(3) {
            leave(a.lua, handle);
        }
        Ok(Value::Nil)
    })?;
    def("TraceHullAttack", |a| {
        let handle = this(a)?;
        let (start, end) = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        let (mins, maxs) = (a.userdata::<Vector>(4, "Vector")?, a.userdata::<Vector>(5, "Vector")?);
        let (amount, kind) = (a.number(6)? as f32, a.opt_number(7)?.unwrap_or(0.0) as i64);
        let force = a.opt_number(8)?.unwrap_or(1.0) as f32;
        let sweep = Sweep { mins, maxs, mask: mask::SHOT, ..Sweep::line(start, end, Filter::Entities(vec![handle])) };
        let hit = trace::trace(a.lua, &sweep);
        if hit.entity.index.is_none() || hit.entity == EntityHandle::WORLD {
            return entity::push(a.lua, EntityHandle::NULL);
        }
        let mut info = DamageInfo::new(amount, kind, handle, handle);
        info.force = (end - start).normalized() * force;
        info.position = hit.end;
        damage::take(a.lua, hit.entity, info);
        entity::push(a.lua, hit.entity)
    })?;
    Ok(())
}

/// Speeds, the hull, keys and the command.
fn install_movement(def: &entity::Def) -> mlua::Result<()> {
    float_field!(def, "GetWalkSpeed", "SetWalkSpeed", walk_speed);
    float_field!(def, "GetRunSpeed", "SetRunSpeed", run_speed);
    float_field!(def, "GetSlowWalkSpeed", "SetSlowWalkSpeed", slow_walk_speed);
    float_field!(def, "GetCrouchedWalkSpeed", "SetCrouchedWalkSpeed", crouched_walk_speed);
    float_field!(def, "GetDuckSpeed", "SetDuckSpeed", duck_speed);
    float_field!(def, "GetUnDuckSpeed", "SetUnDuckSpeed", unduck_speed);
    float_field!(def, "GetJumpPower", "SetJumpPower", jump_power);
    float_field!(def, "GetMaxSpeed", "SetMaxSpeed", max_speed);
    float_field!(def, "GetLadderClimbSpeed", "SetLadderClimbSpeed", ladder_climb_speed);
    float_field!(def, "GetStepSize", "SetStepSize", step_size);
    float_field!(def, "GetLaggedMovementValue", "SetLaggedMovementValue", lagged_movement);
    bool_field!(def, "GetCanWalk", "SetCanWalk", can_walk);
    bool_field!(def, "GetAvoidPlayers", "SetAvoidPlayers", avoid_players);
    bool_field!(def, "GetNoCollideWithTeammates", "SetNoCollideWithTeammates", no_collide_teammates);
    bool_field!(def, "GetAllowFullRotation", "SetAllowFullRotation", allow_full_rotation);
    def("SprintEnable", |a| write(a, |_, p| p.sprint = true))?;
    def("SprintDisable", |a| write(a, |_, p| p.sprint = false))?;
    def("IsSprinting", |a| boolean(read(a, |_, p| p.sprint && p.buttons & key::SPEED != 0)?))?;
    def("IsWalking", |a| boolean(read(a, |_, p| p.can_walk && p.buttons & key::WALK != 0)?))?;
    // what holding the keys does: the next commands decide
    for name in ["StartSprinting", "StopSprinting", "StartWalking", "StopWalking"] {
        def(name, nothing)?;
    }
    def("Crouching", |a| boolean(read(a, |ent, _| ent.flags & fl::DUCKING != 0)?))?;
    def("SetHull", |a| {
        let hull = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        write(a, |ent, p| {
            p.hull = hull;
            if !p.moving.ducked {
                (ent.mins, ent.maxs) = hull;
            }
        })
    })?;
    def("SetHullDuck", |a| {
        let hull = (a.userdata::<Vector>(2, "Vector")?, a.userdata::<Vector>(3, "Vector")?);
        write(a, |ent, p| {
            p.duck_hull = hull;
            if p.moving.ducked {
                (ent.mins, ent.maxs) = hull;
            }
        })
    })?;
    def("ResetHull", |a| {
        let fresh = Settings::default();
        write(a, |ent, p| {
            p.hull = (vector(fresh.hull.0), vector(fresh.hull.1));
            p.duck_hull = (vector(fresh.duck_hull.0), vector(fresh.duck_hull.1));
            (ent.mins, ent.maxs) = if p.moving.ducked { p.duck_hull } else { p.hull };
        })
    })?;
    def("ExitLadder", |a| write(a, |_, p| p.moving.ladder = None))?;
    def("KeyDown", |a| {
        let k = key_arg(a)?;
        boolean(read(a, |_, p| p.buttons & k != 0)?)
    })?;
    def("KeyDownLast", |a| {
        let k = key_arg(a)?;
        boolean(read(a, |_, p| p.old_buttons & k != 0)?)
    })?;
    def("KeyPressed", |a| {
        let k = key_arg(a)?;
        boolean(read(a, |_, p| p.buttons & k != 0 && p.old_buttons & k == 0)?)
    })?;
    def("KeyReleased", |a| {
        let k = key_arg(a)?;
        boolean(read(a, |_, p| p.buttons & k == 0 && p.old_buttons & k != 0)?)
    })?;
    def("GetCurrentCommand", |a| {
        let cmd = read(a, |_, p| p.cmd)?;
        Ok(Value::UserData(a.lua.create_userdata(cmd)?))
    })?;
    def("GetDrivingEntity", |a| {
        let handle = read(a, |_, p| p.driving)?;
        entity::push(a.lua, handle)
    })?;
    def("GetDrivingMode", |a| integer(read(a, |_, p| p.driving_mode)?))?;
    def("IsDrivingEntity", |a| {
        let handle = read(a, |_, p| p.driving)?;
        boolean(host(a.lua).entities.is_valid(handle))
    })?;
    def("SetDrivingEntity", |a| {
        let (target, mode) = (opt_entity(a, 2)?, a.opt_number(3)?.unwrap_or(0.0) as i64);
        write(a, |_, p| {
            p.driving = target;
            p.driving_mode = mode;
        })
    })?;
    Ok(())
}

/// Weapons and ammo.
fn install_weapons(def: &entity::Def) -> mlua::Result<()> {
    def("Give", |a| {
        let handle = this(a)?;
        let (class, no_ammo) = (a.string(2)?, a.boolean(3));
        let item = give(a.lua, handle, &class, no_ammo)?;
        entity::push(a.lua, item)
    })?;
    def("GetWeapons", |a| {
        let weapons = read(a, |_, p| p.weapons.clone())?;
        list(a.lua, weapons.into_iter().filter(|&w| host(a.lua).entities.is_valid(w)))
    })?;
    def("GetWeapon", |a| {
        let handle = this(a)?;
        let class = a.string(2)?;
        entity::push(a.lua, weapon_of(a.lua, handle, &class).unwrap_or(EntityHandle::NULL))
    })?;
    def("HasWeapon", |a| {
        let handle = this(a)?;
        let class = a.string(2)?;
        boolean(weapon_of(a.lua, handle, &class).is_some())
    })?;
    def("GetActiveWeapon", |a| {
        let active = read(a, |_, p| p.active_weapon)?;
        entity::push(a.lua, active)
    })?;
    def("GetPreviousWeapon", |a| {
        let previous = read(a, |_, p| p.previous_weapon)?;
        entity::push(a.lua, previous)
    })?;
    def("SelectWeapon", |a| {
        let handle = this(a)?;
        let item = weapon_arg(a, handle, 2)?;
        switch_weapon(a.lua, handle, item);
        Ok(Value::Nil)
    })?;
    // the active weapon, without `Holster` and `Deploy`
    def("SetActiveWeapon", |a| {
        let item = opt_entity(a, 2)?;
        write(a, |_, p| {
            if p.weapons.contains(&item) || item.index.is_none() {
                p.previous_weapon = p.active_weapon;
                p.active_weapon = item;
            }
        })
    })?;
    def("DropWeapon", |a| {
        let handle = this(a)?;
        let item = match a.get(2) {
            None => read(a, |_, p| p.active_weapon)?,
            Some(_) => weapon_arg(a, handle, 2)?,
        };
        drop_weapon(a.lua, handle, item, opt_vector(a, 4)?);
        Ok(Value::Nil)
    })?;
    def("DropNamedWeapon", |a| {
        let handle = this(a)?;
        let class = a.string(2)?;
        if let Some(item) = weapon_of(a.lua, handle, &class) {
            drop_weapon(a.lua, handle, item, opt_vector(a, 4)?);
        }
        Ok(Value::Nil)
    })?;
    def("PickupWeapon", |a| {
        let handle = this(a)?;
        let item = entity::this(a, 2)?;
        let ammo_only = a.boolean(3);
        let (class, owned) = host(a.lua).entities.get(item).map(|ent| (ent.class.clone(), ent.owner.index.is_some())).unwrap_or_default();
        if item.kind != Kind::Weapon || owned {
            return boolean(false);
        }
        if ammo_only || weapon_of(a.lua, handle, &class).is_some() {
            // its rounds, and it is gone
            let data = weapon::data(a.lua, item).unwrap_or_default();
            let clips = host(a.lua).entities.get(item).and_then(|ent| ent.weapon.as_ref().map(|w| w.clip)).unwrap_or([-1, -1]);
            for (i, clip) in clips.into_iter().enumerate() {
                let rounds = if data.clip_size[i] < 0 { data.default_clip[i] } else { clip };
                give_ammo(a.lua, handle, weapon::ammo_id(a.lua, &data.ammo[i]), rounds);
            }
            crate::ents::remove(a.lua, item);
        } else {
            equip(a.lua, handle, item, false);
        }
        boolean(true)
    })?;
    def("StripWeapon", |a| {
        let handle = this(a)?;
        let class = a.string(2)?;
        if let Some(item) = weapon_of(a.lua, handle, &class) {
            strip_weapon(a.lua, handle, item);
        }
        Ok(Value::Nil)
    })?;
    def("StripWeapons", |a| {
        let handle = this(a)?;
        strip_weapons(a.lua, handle);
        Ok(Value::Nil)
    })?;
    def("RemoveAllItems", |a| {
        let handle = this(a)?;
        strip_weapons(a.lua, handle);
        let ids: Vec<i64> = read(a, |_, p| p.ammo.keys().copied().collect())?;
        for id in ids {
            set_ammo(a.lua, handle, id, 0);
        }
        if a.boolean(2) {
            write(a, |_, p| p.suit = false)?;
        }
        Ok(Value::Nil)
    })?;
    def("GiveAmmo", |a| {
        let handle = this(a)?;
        let amount = a.number(2)? as i64;
        let id = weapon::ammo_arg(a, 3)?;
        integer(give_ammo(a.lua, handle, id, amount))
    })?;
    def("GetAmmoCount", |a| {
        let handle = this(a)?;
        let id = weapon::ammo_arg(a, 2)?;
        integer(ammo_count(a.lua, handle, id))
    })?;
    def("SetAmmo", |a| {
        let handle = this(a)?;
        let count = a.number(2)? as i64;
        let id = weapon::ammo_arg(a, 3)?;
        set_ammo(a.lua, handle, id, count);
        Ok(Value::Nil)
    })?;
    def("RemoveAmmo", |a| {
        let handle = this(a)?;
        let count = a.number(2)? as i64;
        let id = weapon::ammo_arg(a, 3)?;
        remove_ammo(a.lua, handle, id, count);
        Ok(Value::Nil)
    })?;
    for name in ["RemoveAllAmmo", "StripAmmo"] {
        def(name, |a| {
            let handle = this(a)?;
            let ids: Vec<i64> = read(a, |_, p| p.ammo.keys().copied().collect())?;
            for id in ids {
                set_ammo(a.lua, handle, id, 0);
            }
            Ok(Value::Nil)
        })?;
    }
    def("GetAmmo", |a| {
        let ammo = read(a, |_, p| p.ammo.clone())?;
        let t = a.lua.create_table()?;
        for (id, count) in ammo.into_iter().filter(|(_, c)| *c > 0) {
            t.raw_set(id, count)?;
        }
        Ok(Value::Table(t))
    })?;
    def("GetActivity", |a| integer(read(a, |_, p| p.activity)?))?;
    def("SetActivity", |a| {
        let act = a.number(2)? as i64;
        write(a, |_, p| p.activity = act)
    })?;
    // the gamemode's `TranslateActivity` says what the weapon's hold type makes of it
    // the weapon's, not the gamemode's: the base gamemode's `TranslateActivity` asks this
    def("TranslateWeaponActivity", |a| {
        let handle = this(a)?;
        let act = a.number(2)? as i64;
        integer(weapon::translate_activity(a.lua, handle, act))
    })?;
    bool_field!(def, "GetAllowWeaponsInVehicle", "SetAllowWeaponsInVehicle", allow_weapons_in_vehicle);
    def("ShouldDropWeapon", |a| {
        let drop = a.boolean(2);
        write(a, |_, p| p.should_drop_weapon = drop)
    })?;
    def("SetSuppressPickupNotices", |a| {
        let on = a.boolean(2);
        write(a, |_, p| p.suppress_pickup_notices = on)
    })?;
    Ok(())
}

/// The eyes and the view.
fn install_view(def: &entity::Def) -> mlua::Result<()> {
    def("GetAimVector", |a| {
        let angles = read(a, |_, p| p.eye_angles)?;
        entity::vector(a.lua, angles.vectors().0)
    })?;
    def("GetShootPos", |a| {
        let handle = this(a)?;
        let (eye, _) = eye(a.lua, handle).expect("checked above");
        entity::vector(a.lua, eye)
    })?;
    def("LocalEyeAngles", |a| entity::angle(a.lua, read(a, |_, p| p.eye_angles)?))?;
    // the client's view snaps there too: the commands it sends look that way from now on
    def("SetEyeAngles", |a| {
        let angles = a.userdata::<Angle>(2, "Angle")?;
        set_eye_angles(a.lua, this(a)?, angles);
        Ok(Value::Nil)
    })?;
    def("GetEyeTrace", |a| {
        let handle = this(a)?;
        trace::eye_trace(a.lua, handle)
    })?;
    def("GetEyeTraceNoCursor", |a| {
        let handle = this(a)?;
        trace::eye_trace(a.lua, handle)
    })?;
    vector_field!(def, "GetViewOffset", "SetViewOffset", view_offset);
    vector_field!(def, "GetViewOffsetDucked", "SetViewOffsetDucked", view_offset_ducked);
    def("GetCurrentViewOffset", |a| entity::vector(a.lua, Vector::new(0.0, 0.0, read(a, |_, p| p.moving.view_height)?)))?;
    def("SetCurrentViewOffset", |a| {
        let v = a.userdata::<Vector>(2, "Vector")?;
        write(a, |_, p| p.moving.view_height = v.z)
    })?;
    def("GetFOV", |a| number(read(a, |_, p| p.fov)?))?;
    def("SetFOV", |a| {
        let fov = a.number(2)? as f32;
        write(a, |_, p| p.fov = if fov <= 0.0 { DEFAULT_FOV } else { fov })
    })?;
    def("StopZooming", |a| write(a, |_, p| p.fov = DEFAULT_FOV))?;
    bool_field!(def, "GetCanZoom", "SetCanZoom", can_zoom);
    def("GetViewEntity", |a| {
        let handle = this(a)?;
        let view = read(a, |_, p| p.view_entity)?;
        entity::push(a.lua, if host(a.lua).entities.is_valid(view) { view } else { handle })
    })?;
    def("SetViewEntity", |a| {
        let handle = this(a)?;
        let view = opt_entity(a, 2)?;
        write(a, |_, p| p.view_entity = if view == handle { EntityHandle::NULL } else { view })
    })?;
    def("GetViewModel", |a| {
        let index = a.opt_number(2)?.unwrap_or(0.0) as usize;
        let vm = read(a, |_, p| p.view_models.get(index).copied().unwrap_or(EntityHandle::NULL))?;
        entity::push(a.lua, vm)
    })?;
    // whether the player is drawn as others see them: what the gamemode's
    // `ShouldDrawLocalPlayer` says; openGMOD draws the view from the eyes, so not otherwise
    def("ShouldDrawLocalPlayer", |a| {
        let ply = a.get(1).cloned().unwrap_or(Value::Nil);
        let said = crate::call_hook(a.lua, "ShouldDrawLocalPlayer", MultiValue::from_iter([ply]));
        Ok(Value::Boolean(matches!(said.front(), Some(Value::Boolean(true)))))
    })?;
    entity_field!(def, "GetHands", "SetHands", hands);
    entity_field!(def, "GetHoveredWidget", "SetHoveredWidget", hovered_widget);
    entity_field!(def, "GetPressedWidget", "SetPressedWidget", pressed_widget);
    angle_field!(def, "GetViewPunchAngles", "SetViewPunchAngles", view_punch);
    angle_field!(def, "GetViewPunchVelocity", "SetViewPunchVelocity", view_punch_velocity);
    angle_field!(def, "GetRenderAngles", "SetRenderAngles", render_angles);
    def("GetPunchAngle", |a| entity::angle(a.lua, read(a, |_, p| p.view_punch)?))?;
    // as Source does it: the punch goes into the view's spring as velocity
    def("ViewPunch", |a| {
        let punch = a.userdata::<Angle>(2, "Angle")?;
        view_punch(a.lua, this(a)?, punch);
        Ok(Value::Nil)
    })?;
    def("ViewPunchReset", |a| {
        write(a, |_, p| {
            p.view_punch = Angle::default();
            p.view_punch_velocity = Angle::default();
        })
    })?;
    vector_field!(def, "GetPlayerColor", "SetPlayerColor", player_color);
    vector_field!(def, "GetWeaponColor", "SetWeaponColor", weapon_color);
    def("Flashlight", |a| {
        let handle = this(a)?;
        set_flashlight(a.lua, handle, a.boolean(2));
        Ok(Value::Nil)
    })?;
    def("FlashlightIsOn", |a| boolean(read(a, |ent, _| ent.effects & EF_DIMLIGHT != 0)?))?;
    def("CrosshairEnable", |a| write(a, |_, p| p.crosshair = true))?;
    def("CrosshairDisable", |a| write(a, |_, p| p.crosshair = false))?;
    def("DrawViewModel", |a| {
        let draw = a.boolean(2);
        write(a, |_, p| p.draw_view_model = draw)
    })?;
    def("DrawWorldModel", |a| {
        let draw = a.boolean(2);
        write(a, |_, p| p.draw_world_model = draw)
    })?;
    def("DisableWorldClicking", |a| {
        let disable = a.boolean(2);
        write(a, |_, p| p.world_clicking_disabled = disable)
    })?;
    def("IsWorldClickingDisabled", |a| boolean(read(a, |_, p| p.world_clicking_disabled)?))?;
    def("IsWorldClicking", no)?;
    def("SetDSP", |a| {
        let dsp = a.number(2)? as i64;
        write(a, |_, p| p.dsp = dsp)
    })?;
    def("MotionSensorPos", |a| {
        this(a)?;
        entity::vector(a.lua, Vector::default())
    })?;
    Ok(())
}

/// Chat and messages, and what has nothing to act on yet.
fn install_rest(def: &entity::Def) -> mlua::Result<()> {
    def("ChatPrint", |a| {
        let handle = this(a)?;
        crate::chat::tell(a.lua, handle, &a.string(2)?);
        Ok(Value::Nil)
    })?;
    // `HUD_PRINTTALK` goes to the chat, the rest to the console
    def("PrintMessage", |a| {
        let handle = this(a)?;
        let text = a.string(3)?;
        if a.number(2)? as i64 == HUD_PRINTTALK {
            crate::chat::tell(a.lua, handle, &text);
        } else {
            print_to(a, vec![handle], &text);
        }
        Ok(Value::Nil)
    })?;
    // what the player says goes through `PlayerSay`, which may change it or keep it quiet
    def("Say", |a| {
        let handle = this(a)?;
        crate::chat::say(a.lua, handle, &a.string(2)?, a.boolean(3));
        Ok(Value::Nil)
    })?;
    // for the player's client: it runs the line, and sends the server what it does not know
    def("ConCommand", |a| {
        let (handle, line) = (this(a)?, a.string(2)?);
        to_client(a.lua, handle, ToClient::Command(line));
        Ok(Value::Nil)
    })?;
    def("SendLua", |a| {
        let (handle, code) = (this(a)?, a.string(2)?);
        to_client(a.lua, handle, ToClient::Lua(code));
        Ok(Value::Nil)
    })?;
    for name in ["SendHint", "SuppressHint", "ScreenFade", "PlayStepSound", "SprayDecal"] {
        def(name, nothing)?;
    }
    def("AllowImmediateDecalPainting", nothing)?;
    // the gesture slots and the animation events of the player's model
    crate::anim::install_player(def)?;
    crate::pickup::install_player(def)?;
    crate::lagcomp::install_player(def)?;
    // the base gamemode makes the physics gun's own in Lua (`AddFrozenPhysicsObject`,
    // `PhysgunUnfreeze`, `UnfreezePhysicsObjects`); these stand in without it. Nothing is
    // carried with the gravity gun yet, and there are no vehicles.
    for name in ["AddFrozenPhysicsObject", "SimulateGravGunDrop", "SimulateGravGunPickup", "DetonateTripmines"] {
        def(name, nothing)?;
    }
    def("PhysgunUnfreeze", zero)?;
    def("UnfreezePhysicsObjects", nothing)?;
    def("GetPreferredCarryAngles", nothing)?;
    // what the player carries with the use key
    def("GetEntityInUse", |a| {
        let handle = this(a)?;
        entity::push(a.lua, crate::pickup::carrying(a.lua, handle).unwrap_or(EntityHandle::NULL))
    })?;
    def("GetUseEntity", null)?;
    Ok(())
}
