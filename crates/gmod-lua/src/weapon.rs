//! Weapons: the ammo types of the game, what the engine knows of each kind of weapon (its
//! clips, ammo, slot and name: from `scripts/weapons/<class>.txt` for the engine's own, from
//! the SWEP table for scripted ones), what it keeps for each weapon entity (the rounds in its
//! clips, when it may fire next), and the methods of `Weapon`.
//!
//! A weapon in a player's hands works as in Source: every tick the active one gets its
//! `Think`, and the keys held fire it - `PrimaryAttack` when `IN_ATTACK` is down and its next
//! primary fire has come (pressed anew, for a SWEP that is not `Primary.Automatic`),
//! `SecondaryAttack` for `IN_ATTACK2`, `Reload` for `IN_RELOAD`. The engine's own weapons fire
//! as Half-Life 2's do ([`crate::hl2weapons`]). A client does the same for its own player's
//! weapon in the ticks it predicts ([`crate::player::predict`]).

use std::cell::RefCell;
use std::collections::HashMap;

use mlua::{Lua, MultiValue, ObjectLike, Table, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::{files, host};

/// The most of one ammo type a player carries: `gmod_maxammo`, which GMod sets for every type.
pub const MAX_AMMO: i64 = 9999;

/// `DMG_` types of the default ammo types.
mod dmg {
    pub const BULLET: i64 = 2;
    pub const SLASH: i64 = 4;
    pub const BURN: i64 = 8;
    pub const BLAST: i64 = 64;
    pub const CLUB: i64 = 128;
    pub const SHOCK: i64 = 256;
    pub const ENERGYBEAM: i64 = 1024;
    pub const SONIC: i64 = 2048;
    pub const AIRBOAT: i64 = 33554432;
    pub const DISSOLVE: i64 = 67108864;
    pub const BUCKSHOT: i64 = 536870912;
    pub const SNIPER: i64 = 1073741824;
}

/// The ammo types the engine has, in the order of their IDs (from 1), with the damage type of
/// each: HL2's and HL:S's, as GMod's wiki lists them (`Default_Ammo_Types`).
const DEFAULT_AMMO: &[(&str, i64)] = &[
    ("AR2", dmg::BULLET),
    ("AR2AltFire", dmg::DISSOLVE),
    ("Pistol", dmg::BULLET),
    ("SMG1", dmg::BULLET),
    ("357", dmg::BULLET),
    ("XBowBolt", dmg::BULLET),
    ("Buckshot", dmg::BULLET | dmg::BUCKSHOT),
    ("RPG_Round", dmg::BURN),
    ("SMG1_Grenade", dmg::BURN),
    ("Grenade", dmg::BURN),
    ("slam", dmg::BURN),
    ("AlyxGun", dmg::BULLET),
    ("SniperRound", dmg::BULLET | dmg::SNIPER),
    ("SniperPenetratedRound", dmg::BULLET | dmg::SNIPER),
    ("Thumper", dmg::SONIC),
    ("Gravity", dmg::CLUB),
    ("Battery", dmg::CLUB),
    ("GaussEnergy", dmg::SHOCK),
    ("CombineCannon", dmg::BULLET),
    ("AirboatGun", dmg::AIRBOAT),
    ("StriderMinigun", dmg::BULLET),
    ("HelicopterGun", dmg::BULLET),
    ("9mmRound", dmg::BULLET),
    ("357Round", dmg::BULLET),
    ("BuckshotHL1", dmg::BULLET | dmg::BUCKSHOT),
    ("XBowBoltHL1", dmg::BULLET),
    ("MP5_Grenade", dmg::BLAST),
    ("RPG_Rocket", dmg::BLAST),
    ("Uranium", dmg::ENERGYBEAM),
    ("GrenadeHL1", dmg::BLAST),
    ("Hornet", dmg::BULLET),
    ("Snark", dmg::SLASH),
    ("TripMine", dmg::BLAST),
    ("Satchel", dmg::BLAST),
    ("12mmRound", dmg::BULLET),
    ("StriderMinigunDirect", dmg::BULLET),
    ("CombineHeavyCannon", dmg::BULLET),
];

/// An ammo type. The damage of the engine's own types comes from the skill console variables
/// (`sk_plr_dmg_pistol` ...) in Source, which are not read yet: it is 0 here.
#[derive(Debug, Clone, PartialEq)]
pub struct AmmoType {
    pub name: String,
    pub dmg_type: i64,
    pub tracer: i64,
    pub player_damage: f64,
    pub npc_damage: f64,
    pub force: f64,
    pub max_carry: i64,
    pub min_splash: f64,
    pub max_splash: f64,
    pub flags: i64,
}

impl AmmoType {
    fn engine(name: &str, dmg_type: i64) -> AmmoType {
        AmmoType {
            name: name.to_string(),
            dmg_type,
            tracer: 0,
            player_damage: 0.0,
            npc_damage: 0.0,
            force: 0.0,
            max_carry: MAX_AMMO,
            min_splash: 4.0,
            max_splash: 8.0,
            flags: 0,
        }
    }

    /// One of `game.AddAmmoType`'s tables.
    fn from_table(t: &Table) -> mlua::Result<Option<AmmoType>> {
        let Some(name) = t.get::<Option<String>>("name")? else { return Ok(None) };
        let number = |key: &str| -> mlua::Result<f64> { Ok(t.get::<Option<f64>>(key)?.unwrap_or(0.0)) };
        Ok(Some(AmmoType {
            name,
            dmg_type: number("dmgtype")? as i64,
            tracer: number("tracer")? as i64,
            player_damage: number("plydmg")?,
            npc_damage: number("npcdmg")?,
            force: number("force")?,
            // GMod's `gmod_maxammo` holds for every type, whatever `maxcarry` says
            max_carry: MAX_AMMO,
            min_splash: t.get::<Option<f64>>("minsplash")?.unwrap_or(4.0),
            max_splash: t.get::<Option<f64>>("maxsplash")?.unwrap_or(8.0),
            flags: number("flags")? as i64,
        }))
    }
}

/// What the engine knows of a kind of weapon.
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponData {
    pub print_name: String,
    pub view_model: String,
    pub world_model: String,
    pub hold_type: String,
    pub slot: i64,
    pub slot_pos: i64,
    pub weight: i64,
    /// Primary and secondary: rounds a clip holds (-1: no clip, the rounds are used from the
    /// owner's ammo), rounds a new weapon comes with, and the name of the ammo type.
    pub clip_size: [i64; 2],
    pub default_clip: [i64; 2],
    pub ammo: [String; 2],
    pub auto_switch_to: bool,
    pub auto_switch_from: bool,
    /// Fires while the key is held rather than once a press (a SWEP's `Primary.Automatic`).
    pub automatic: [bool; 2],
    /// An engine weapon's icons in the row of slots, as its script's `TextureData` has them: a
    /// font of the scheme and the character of it, and the same when chosen.
    pub icons: [Option<(String, String)>; 2],
    /// An engine weapon's sounds by what they are for (`single_shot`, `empty`, `reload`,
    /// `melee_hit` ...): sound scripts.
    pub sounds: Vec<(String, String)>,
}

impl WeaponData {
    /// The sound script of what an engine weapon does (`single_shot`), if it has one.
    pub fn sound(&self, what: &str) -> Option<&str> {
        self.sounds.iter().find(|(k, _)| k.eq_ignore_ascii_case(what)).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }
}

impl Default for WeaponData {
    fn default() -> WeaponData {
        WeaponData {
            print_name: String::new(),
            view_model: String::new(),
            world_model: String::new(),
            hold_type: String::new(),
            slot: 0,
            slot_pos: 10,
            weight: 5,
            clip_size: [-1, -1],
            default_clip: [0, 0],
            ammo: [String::new(), String::new()],
            auto_switch_to: true,
            auto_switch_from: true,
            automatic: [true, true],
            icons: [None, None],
            sounds: Vec::new(),
        }
    }
}

/// What the engine keeps for a weapon entity.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct WeaponState {
    /// Rounds in the primary and secondary clip; -1 for no clip.
    pub clip: [i64; 2],
    /// When it may fire next (`CurTime`).
    pub next_fire: [f64; 2],
    pub activity: i64,
    pub hold_type: String,
    pub last_shoot: f64,
    pub deploy_speed: f32,
    /// Reloading (`DefaultReload`): when the clip fills.
    pub reload: Option<f64>,
    /// When what the view model was last sent has played, and it may go back to idle.
    pub idle_time: f64,
}

/// The activities of the player's model a hold type plays, from the first (`ACT_HL2MP_IDLE_*`),
/// for those the player's animation asks for: standing, walking, running, crouching and
/// walking so, shooting either way, reloading either way, jumping, swimming still and moving.
const HOLD_ACTIVITIES: &[(&str, i64)] = &[
    ("ACT_MP_STAND_IDLE", 0),
    ("ACT_MP_WALK", 1),
    ("ACT_MP_RUN", 2),
    ("ACT_MP_CROUCH_IDLE", 3),
    ("ACT_MP_CROUCHWALK", 4),
    ("ACT_MP_ATTACK_STAND_PRIMARYFIRE", 5),
    ("ACT_MP_ATTACK_CROUCH_PRIMARYFIRE", 5),
    ("ACT_MP_RELOAD_STAND", 6),
    ("ACT_MP_RELOAD_CROUCH", 6),
    ("ACT_MP_JUMP", 7),
    ("ACT_RANGE_ATTACK1", 8),
    ("ACT_MP_SWIM", 9),
];

/// What a player's weapon in hand makes of an activity of the player's model
/// (`Player:TranslateWeaponActivity`): a SWEP's `TranslateActivity`, an engine weapon's by its
/// hold type. What it does not translate (-1, nothing) stays as it was.
pub(crate) fn translate_activity(lua: &Lua, player: EntityHandle, act: i64) -> i64 {
    let Some(active) = crate::player::state(lua, player, |_, p| p.active_weapon) else { return act };
    let Some((scripted, hold)) = host(lua)
        .entities
        .get(active)
        .filter(|e| !e.removed)
        .map(|e| (e.scripted, e.weapon.as_ref().map(|w| w.hold_type.clone()).unwrap_or_default()))
    else {
        return act;
    };
    let number = |value: Option<&Value>| match value {
        Some(Value::Integer(n)) => Some(*n),
        Some(Value::Number(n)) => Some(*n as i64),
        _ => None,
    };
    if scripted {
        let Ok(Value::UserData(ud)) = entity::push(lua, active) else { return act };
        let translated = crate::ents::call_method(lua, &ud, "TranslateActivity", MultiValue::from_iter([Value::Integer(act)]));
        return translated.and_then(|values| number(values.front())).filter(|&n| n >= 0).unwrap_or(act);
    }
    let globals = lua.globals();
    let enum_value = |name: &str| number(globals.raw_get::<Value>(name).ok().as_ref());
    let suffix = match hold.as_str() {
        "" | "normal" => String::new(),
        "smg" => "_SMG1".to_string(),
        other => format!("_{}", other.to_ascii_uppercase()),
    };
    let Some(first) = enum_value(&format!("ACT_HL2MP_IDLE{suffix}")) else { return act };
    HOLD_ACTIVITIES.iter().find(|(name, _)| enum_value(name) == Some(act)).map_or(act, |(_, offset)| first + offset)
}

/// Weapon scripts and ammo types, kept with the [`crate::Host`].
#[derive(Default)]
pub(crate) struct Weapons {
    pub(crate) scripts: RefCell<HashMap<String, Option<WeaponData>>>,
    /// The ammo types once the game has started; until then they are built when asked for.
    ammo: RefCell<Option<Vec<AmmoType>>>,
    /// The skill settings of the game's `cfg/skill.cfg` (`sk_plr_dmg_pistol` ...), read when
    /// first asked for.
    skill: RefCell<Option<HashMap<String, f32>>>,
}

/// The skill settings of the engine's ammo and weapons: (the name in `sk_plr_dmg_*`, what a
/// player's does, what an NPC's does) - about what Half-Life 2 does on normal skill, for a
/// game without its `skill.cfg`.
const SKILL_DAMAGE: &[(&str, &str, f32, f32)] = &[
    ("Pistol", "pistol", 8.0, 3.0),
    ("SMG1", "smg1", 4.0, 3.0),
    ("AR2", "ar2", 8.0, 3.0),
    ("357", "357", 40.0, 30.0),
    ("Buckshot", "buckshot", 8.0, 3.0),
    ("XBowBolt", "crossbow", 100.0, 10.0),
    ("AlyxGun", "alyxgun", 5.0, 3.0),
    ("SniperRound", "sniper_round", 100.0, 100.0),
    ("SniperPenetratedRound", "sniper_round", 100.0, 100.0),
    ("CombineCannon", "combine_cannon", 3.0, 3.0),
    ("AirboatGun", "airboat", 3.0, 3.0),
    ("StriderMinigun", "strider", 5.0, 5.0),
    ("HelicopterGun", "helicopter", 3.0, 3.0),
    // the melee weapons', by their class
    ("weapon_crowbar", "crowbar", 10.0, 10.0),
    ("weapon_stunstick", "stunstick", 40.0, 40.0),
];

/// A skill setting: a ConVar of that name, else the game's `cfg/skill.cfg`.
pub(crate) fn skill(lua: &Lua, name: &str) -> Option<f32> {
    if let Some(value) = crate::convar::value(lua, name) {
        return Some(crate::convar::atof(&value) as f32);
    }
    let host = host(lua);
    let mut cache = host.weapons.skill.borrow_mut();
    let settings = cache.get_or_insert_with(|| {
        let text = host.vfs.read("cfg/skill.cfg", "GAME").map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let mut out = HashMap::new();
        for line in text.lines() {
            let line = line.split("//").next().unwrap_or("");
            let mut words = line.split_whitespace().map(|w| w.trim_matches('"'));
            if let (Some(key), Some(value)) = (words.next(), words.next())
                && let Ok(value) = value.parse::<f32>()
            {
                out.insert(key.to_ascii_lowercase(), value);
            }
        }
        out
    });
    settings.get(&name.to_ascii_lowercase()).copied()
}

/// What a round of an engine's ammo type (or a hit of an engine's melee weapon, by class)
/// does to what it hits: a player's or an NPC's. 0 for what the game has no setting of.
pub(crate) fn skill_damage(lua: &Lua, name: &str, by_player: bool) -> f32 {
    let Some(&(_, key, player, npc)) = SKILL_DAMAGE.iter().find(|(n, ..)| n.eq_ignore_ascii_case(name)) else { return 0.0 };
    let who = if by_player { "plr" } else { "npc" };
    skill(lua, &format!("sk_{who}_dmg_{key}")).unwrap_or(if by_player { player } else { npc })
}

/// The hold types of the engine's weapons in GMod's player animations.
fn engine_hold_type(class: &str) -> &'static str {
    match class {
        "weapon_pistol" | "weapon_alyxgun" => "pistol",
        "weapon_357" => "revolver",
        "weapon_smg1" => "smg",
        "weapon_ar2" => "ar2",
        "weapon_shotgun" | "weapon_annabelle" => "shotgun",
        "weapon_crossbow" => "crossbow",
        "weapon_rpg" => "rpg",
        "weapon_frag" | "weapon_bugbait" => "grenade",
        "weapon_crowbar" | "weapon_stunstick" => "melee",
        "weapon_physcannon" | "weapon_physgun" => "physgun",
        "weapon_slam" => "slam",
        _ => "normal",
    }
}

/// An engine weapon's data, from `scripts/weapons/<class>.txt` (`WeaponData`).
fn script(lua: &Lua, class: &str) -> Option<WeaponData> {
    let class = class.to_ascii_lowercase();
    if let Some(data) = host(lua).weapons.scripts.borrow().get(&class) {
        return data.clone();
    }
    let read = || -> Option<WeaponData> {
        let bytes = host(lua).vfs.read(&format!("scripts/weapons/{class}.txt"), "GAME").ok()?;
        let (document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&bytes), gmod_kv::Options::default());
        let block = document.root.iter().find_map(|pair| pair.value.as_block())?;
        let text = |key: &str| block.str(key).unwrap_or_default().to_string();
        let number = |key: &str, default: i64| block.str(key).map_or(default, |v| crate::convar::atof(v) as i64);
        let ammo = |key: &str| {
            let name = text(key);
            if name.eq_ignore_ascii_case("none") { String::new() } else { name }
        };
        Some(WeaponData {
            print_name: text("printname"),
            view_model: text("viewmodel"),
            world_model: text("playermodel"),
            hold_type: engine_hold_type(&class).to_string(),
            slot: number("bucket", 0),
            slot_pos: number("bucket_position", 0),
            weight: number("weight", 0),
            clip_size: [number("clip_size", -1), number("clip2_size", -1)],
            default_clip: [number("default_clip", 0), number("default_clip2", 0)],
            ammo: [ammo("primary_ammo"), ammo("secondary_ammo")],
            auto_switch_to: number("autoswitchto", 1) != 0,
            auto_switch_from: number("autoswitchfrom", 1) != 0,
            automatic: [true, true],
            icons: ["weapon", "weapon_s"].map(|name| {
                let icon = block.block("TextureData")?.block(name)?;
                Some((icon.str("font")?.to_string(), icon.str("character")?.to_string()))
            }),
            sounds: block
                .block("SoundData")
                .map(|sounds| sounds.iter().filter_map(|p| Some((p.key.clone(), p.value.as_str()?.to_string()))).collect())
                .unwrap_or_default(),
        })
    };
    let data = read();
    host(lua).weapons.scripts.borrow_mut().insert(class, data.clone());
    data
}

/// A SWEP's data, from its table as scripts left it.
fn swep(table: &Table) -> mlua::Result<WeaponData> {
    let defaults = WeaponData::default();
    let text = |key: &str| -> mlua::Result<String> { Ok(table.get::<Option<String>>(key)?.unwrap_or_default()) };
    let number = |key: &str, default: i64| -> mlua::Result<i64> { Ok(table.get::<Option<f64>>(key)?.map_or(default, |n| n as i64)) };
    let flag = |key: &str| -> mlua::Result<bool> { Ok(!matches!(table.get::<Value>(key)?, Value::Boolean(false))) };
    let mut data = WeaponData {
        print_name: text("PrintName")?,
        view_model: text("ViewModel")?,
        world_model: text("WorldModel")?,
        hold_type: text("HoldType")?,
        slot: number("Slot", defaults.slot)?,
        slot_pos: number("SlotPos", defaults.slot_pos)?,
        weight: number("Weight", defaults.weight)?,
        auto_switch_to: flag("AutoSwitchTo")?,
        auto_switch_from: flag("AutoSwitchFrom")?,
        ..defaults
    };
    for (i, key) in ["Primary", "Secondary"].into_iter().enumerate() {
        let Some(part) = table.get::<Option<Table>>(key)? else { continue };
        data.clip_size[i] = part.get::<Option<f64>>("ClipSize")?.map_or(-1, |n| n as i64);
        data.default_clip[i] = part.get::<Option<f64>>("DefaultClip")?.map_or(0, |n| n as i64);
        data.ammo[i] = part.get::<Option<String>>("Ammo")?.filter(|a| !a.eq_ignore_ascii_case("none")).unwrap_or_default();
        data.automatic[i] = part.get::<Option<bool>>("Automatic")?.unwrap_or(false);
    }
    Ok(data)
}

/// The data of a weapon entity: its SWEP table's, or its class's script.
pub(crate) fn data(lua: &Lua, handle: EntityHandle) -> Option<WeaponData> {
    let (class, scripted) = host(lua).entities.get(handle).map(|ent| (ent.class.clone(), ent.scripted))?;
    if !scripted {
        return Some(script(lua, &class).unwrap_or_default());
    }
    let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { return None };
    let table = entity::table_of(&ud).ok()??;
    swep(&table).ok()
}

/// Gives a new weapon entity what the engine keeps for it, with the rounds it comes with in
/// its clips (`GiveDefaultAmmo`), in `COLLISION_GROUP_WEAPON`, which players go through.
pub(crate) fn setup(lua: &Lua, handle: EntityHandle) {
    let data = data(lua, handle).unwrap_or_default();
    let clip = |i: usize| if data.clip_size[i] >= 0 { data.default_clip[i] } else { -1 };
    let state = WeaponState { clip: [clip(0), clip(1)], hold_type: data.hold_type.clone(), deploy_speed: 1.0, ..WeaponState::default() };
    host(lua).entities.with_mut(handle, |ent| {
        ent.weapon = Some(state);
        ent.collision_group = crate::trace::group::WEAPON;
    });
}

/// Every ammo type, the engine's and those of `game.AddAmmoType` (which
/// `game.BuildAmmoTypes`, in the game's Lua, hands over sorted by name).
fn build_ammo(lua: &Lua) -> Vec<AmmoType> {
    let mut list: Vec<AmmoType> = DEFAULT_AMMO.iter().map(|(name, dmg)| AmmoType::engine(name, *dmg)).collect();
    let custom = || -> mlua::Result<Option<Table>> {
        let Some(game) = lua.globals().get::<Option<Table>>("game")? else { return Ok(None) };
        let Some(build) = game.get::<Option<mlua::Function>>("BuildAmmoTypes")? else { return Ok(None) };
        Ok(files::call_reporting(lua, build, MultiValue::new()).ok().and_then(|r| match r.into_iter().next() {
            Some(Value::Table(t)) => Some(t),
            _ => None,
        }))
    };
    if let Ok(Some(table)) = custom() {
        for value in table.sequence_values::<Table>().flatten() {
            let Ok(Some(ammo)) = AmmoType::from_table(&value) else { continue };
            match list.iter_mut().find(|a| a.name.eq_ignore_ascii_case(&ammo.name)) {
                Some(existing) => *existing = ammo,
                None => list.push(ammo),
            }
        }
    }
    list
}

/// Fixes the ammo types, as the engine does once the game has started: `game.AddAmmoType`
/// after that adds nothing.
pub(crate) fn freeze_ammo(lua: &Lua) {
    let list = build_ammo(lua);
    *host(lua).weapons.ammo.borrow_mut() = Some(list);
}

fn with_ammo<R>(lua: &Lua, f: impl FnOnce(&[AmmoType]) -> R) -> R {
    if let Some(list) = host(lua).weapons.ammo.borrow().as_ref() {
        return f(list);
    }
    f(&build_ammo(lua))
}

/// The ID of an ammo type by name (ASCII case ignored), -1 for none.
pub(crate) fn ammo_id(lua: &Lua, name: &str) -> i64 {
    if name.is_empty() {
        return -1;
    }
    with_ammo(lua, |list| list.iter().position(|a| a.name.eq_ignore_ascii_case(name)).map_or(-1, |i| i as i64 + 1))
}

fn ammo_type(lua: &Lua, id: i64) -> Option<AmmoType> {
    with_ammo(lua, |list| usize::try_from(id - 1).ok().and_then(|i| list.get(i).cloned()))
}

/// An ammo type by its ID (from 1).
pub(crate) fn ammo_type_of(lua: &Lua, id: i64) -> Option<AmmoType> {
    ammo_type(lua, id)
}

/// The most of an ammo type a player carries.
pub(crate) fn ammo_max(lua: &Lua, id: i64) -> i64 {
    ammo_type(lua, id).map_or(-1, |a| a.max_carry)
}

/// An ammo type argument: its ID, or its name.
pub(crate) fn ammo_arg(a: &Args, position: usize) -> mlua::Result<i64> {
    match a.get(position) {
        Some(Value::String(name)) => Ok(ammo_id(a.lua, &name.to_string_lossy())),
        Some(Value::Integer(n)) => Ok(*n),
        Some(Value::Number(n)) => Ok(*n as i64),
        _ => Err(a.bad(position, "number")),
    }
}

/// The weapon in argument 1 and what the engine keeps for it.
fn state(a: &Args) -> mlua::Result<(EntityHandle, WeaponState)> {
    let handle = entity::this(a, 1)?;
    let state = host(a.lua).entities.get(handle).and_then(|ent| ent.weapon.clone()).unwrap_or_default();
    Ok((handle, state))
}

fn change(a: &Args, f: impl FnOnce(&mut WeaponState)) -> mlua::Result<Value> {
    let handle = entity::this(a, 1)?;
    host(a.lua).entities.with_mut(handle, |ent| {
        if let Some(state) = ent.weapon.as_mut() {
            f(state);
        }
    });
    Ok(Value::Nil)
}

fn weapon_data(a: &Args) -> mlua::Result<WeaponData> {
    let handle = entity::this(a, 1)?;
    Ok(data(a.lua, handle).unwrap_or_default())
}

fn string(lua: &Lua, text: &str) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(text)?))
}

/// Calls a SWEP's function on a weapon (`self:name(...)`); what it returned, or nothing.
pub(crate) fn call(lua: &Lua, weapon: EntityHandle, name: &str, args: MultiValue) -> Option<MultiValue> {
    let scripted = host(lua).entities.get(weapon).is_some_and(|ent| ent.scripted);
    if !scripted {
        return None;
    }
    let Ok(Value::UserData(ud)) = entity::push(lua, weapon) else { return None };
    let function = match ud.get::<Value>(name) {
        Ok(Value::Function(f)) => f,
        _ => return None,
    };
    let mut full = MultiValue::from_iter([Value::UserData(ud)]);
    full.extend(args);
    files::call_reporting(lua, function, full).ok()
}

/// One tick of the active weapon of a player (`ItemPostFrame`): `Think`, then what the keys
/// held ask for.
pub(crate) fn post_frame(lua: &Lua, weapon: EntityHandle, buttons: u32, pressed: u32) {
    const IN_ATTACK: u32 = 1;
    const IN_ATTACK2: u32 = 2048;
    const IN_RELOAD: u32 = 8192;
    let now = host(lua).clock.cur_time.get();
    // a reload that has run its time fills the clip
    let reload = host(lua).entities.get(weapon).and_then(|ent| ent.weapon.as_ref().and_then(|w| w.reload));
    if let Some(done) = reload {
        if done > now {
            return;
        }
        finish_reload(lua, weapon);
    }
    call(lua, weapon, "Think", MultiValue::new());
    // the engine's own weapons fire as Half-Life 2's do
    if host(lua).entities.get(weapon).is_some_and(|ent| !ent.scripted) {
        crate::hl2weapons::frame(lua, weapon, buttons, pressed);
        crate::anim::weapon_idle(lua, weapon, buttons & (IN_ATTACK | IN_ATTACK2 | IN_RELOAD) != 0);
        return;
    }
    let Some(data) = data(lua, weapon) else { return };
    let Some(state) = host(lua).entities.get(weapon).and_then(|ent| ent.weapon.clone()) else { return };
    let wants = |key: u32, automatic: bool| if automatic { buttons & key != 0 } else { pressed & key != 0 };
    let mut fired = false;
    if wants(IN_ATTACK2, data.automatic[1]) && state.next_fire[1] <= now {
        call(lua, weapon, "SecondaryAttack", MultiValue::new());
        fired = true;
    }
    if !fired && wants(IN_ATTACK, data.automatic[0]) && state.next_fire[0] <= now {
        call(lua, weapon, "PrimaryAttack", MultiValue::new());
    }
    if buttons & IN_RELOAD != 0 {
        call(lua, weapon, "Reload", MultiValue::new());
    }
    crate::anim::weapon_idle(lua, weapon, buttons & (IN_ATTACK | IN_ATTACK2 | IN_RELOAD) != 0);
}

/// Sets a weapon's activity (`SendWeaponAnim`).
pub(crate) fn set_activity(lua: &Lua, weapon: EntityHandle, act: i64) {
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            w.activity = act;
        }
    });
}

/// From when a weapon's view model may go back to idle.
pub(crate) fn set_idle_time(lua: &Lua, weapon: EntityHandle, time: f64) {
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            w.idle_time = time;
        }
    });
}

/// A weapon's activity, and from when it may go back to idle.
pub(crate) fn activity_and_idle(lua: &Lua, weapon: EntityHandle) -> Option<(i64, f64)> {
    host(lua).entities.get(weapon).and_then(|ent| ent.weapon.as_ref().map(|w| (w.activity, w.idle_time)))
}

/// Starts a reload when the clip is not full and the owner has rounds for it
/// (`DefaultReload`): the view model plays the activity, the owner the reload of their own
/// model, and the clip fills when the view model's sequence has played. Whether it started.
pub(crate) fn default_reload(lua: &Lua, weapon: EntityHandle, act: i64) -> bool {
    let Some(state) = host(lua).entities.get(weapon).and_then(|ent| ent.weapon.clone()) else { return false };
    let data = data(lua, weapon).unwrap_or_default();
    let owner = host(lua).entities.get(weapon).map_or(EntityHandle::NULL, |ent| ent.owner);
    let room = |i: usize| {
        data.clip_size[i] >= 0
            && state.clip[i] < data.clip_size[i]
            && crate::player::ammo_count(lua, owner, ammo_id(lua, &data.ammo[i])) > 0
    };
    if state.reload.is_some() || !(room(0) || room(1)) {
        return false;
    }
    let played = crate::anim::send_weapon_anim(lua, weapon, act);
    crate::anim::animation_event(lua, owner, crate::anim::EVENT_RELOAD, 0);
    let duration = if played { crate::anim::view_model_duration(lua, weapon) } else { 0.0 };
    let done = host(lua).clock.cur_time.get() + duration;
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            w.activity = act;
            w.reload = Some(done);
            w.next_fire = [done, done];
        }
    });
    true
}

/// The end of a reload: the clip takes what it holds from the owner's ammo.
fn finish_reload(lua: &Lua, weapon: EntityHandle) {
    let data = data(lua, weapon).unwrap_or_default();
    let owner = host(lua).entities.get(weapon).map_or(EntityHandle::NULL, |ent| ent.owner);
    for i in 0..2 {
        let id = ammo_id(lua, &data.ammo[i]);
        let Some(clip) = host(lua).entities.get(weapon).and_then(|ent| ent.weapon.as_ref().map(|w| w.clip[i])) else { return };
        if data.clip_size[i] < 0 || clip < 0 {
            continue;
        }
        let take = (data.clip_size[i] - clip).min(crate::player::ammo_count(lua, owner, id)).max(0);
        crate::player::remove_ammo(lua, owner, id, take);
        host(lua).entities.with_mut(weapon, |ent| {
            if let Some(w) = ent.weapon.as_mut() {
                w.clip[i] += take;
            }
        });
    }
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            w.reload = None;
        }
    });
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let game: Table = lua.globals().get("game")?;
    let def = |table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def(&game, "GetAmmoID", |a| Ok(Value::Integer(ammo_id(a.lua, &a.string(1)?))))?;
    def(&game, "GetAmmoName", |a| match ammo_type(a.lua, a.number(1)? as i64) {
        Some(ammo) => string(a.lua, &ammo.name),
        None => Ok(Value::Nil),
    })?;
    def(&game, "GetAmmoTypes", |a| {
        let names = with_ammo(a.lua, |list| list.iter().map(|ammo| ammo.name.clone()).collect::<Vec<_>>());
        let t = a.lua.create_table()?;
        for (i, name) in names.into_iter().enumerate() {
            t.raw_set(i + 1, name)?;
        }
        Ok(Value::Table(t))
    })?;
    def(&game, "GetAmmoMax", |a| Ok(Value::Integer(ammo_max(a.lua, a.number(1)? as i64))))?;
    def(&game, "GetAmmoDamageType", |a| Ok(Value::Integer(ammo_type(a.lua, a.number(1)? as i64).map_or(0, |t| t.dmg_type))))?;
    def(&game, "GetAmmoForce", |a| Ok(Value::Number(ammo_type(a.lua, a.number(1)? as i64).map_or(0.0, |t| t.force))))?;
    def(&game, "GetAmmoPlayerDamage", |a| Ok(Value::Number(ammo_type(a.lua, a.number(1)? as i64).map_or(0.0, |t| t.player_damage))))?;
    def(&game, "GetAmmoNPCDamage", |a| Ok(Value::Number(ammo_type(a.lua, a.number(1)? as i64).map_or(0.0, |t| t.npc_damage))))?;
    def(&game, "GetAmmoData", |a| {
        let Some(ammo) = ammo_type(a.lua, a.number(1)? as i64) else { return Ok(Value::Nil) };
        let t = a.lua.create_table()?;
        t.raw_set("name", ammo.name)?;
        t.raw_set("dmgtype", ammo.dmg_type)?;
        t.raw_set("tracer", ammo.tracer)?;
        t.raw_set("plydmg", ammo.player_damage)?;
        t.raw_set("npcdmg", ammo.npc_damage)?;
        t.raw_set("force", ammo.force)?;
        t.raw_set("maxcarry", ammo.max_carry)?;
        t.raw_set("minsplash", ammo.min_splash)?;
        t.raw_set("maxsplash", ammo.max_splash)?;
        t.raw_set("flags", ammo.flags)?;
        Ok(Value::Table(t))
    })?;

    let meta = crate::meta::meta_table(lua, "Weapon", crate::meta::type_id::ENTITY)?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| def(&meta, name, f);
    def("IsScripted", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(host(a.lua).entities.get(handle).is_some_and(|ent| ent.scripted)))
    })?;
    def("Clip1", |a| Ok(Value::Integer(state(a)?.1.clip[0])))?;
    def("Clip2", |a| Ok(Value::Integer(state(a)?.1.clip[1])))?;
    def("SetClip1", |a| {
        let n = a.number(2)? as i64;
        change(a, |w| w.clip[0] = n)
    })?;
    def("SetClip2", |a| {
        let n = a.number(2)? as i64;
        change(a, |w| w.clip[1] = n)
    })?;
    def("GetMaxClip1", |a| Ok(Value::Integer(weapon_data(a)?.clip_size[0])))?;
    def("GetMaxClip2", |a| Ok(Value::Integer(weapon_data(a)?.clip_size[1])))?;
    def("GetPrimaryAmmoType", |a| {
        let data = weapon_data(a)?;
        Ok(Value::Integer(ammo_id(a.lua, &data.ammo[0])))
    })?;
    def("GetSecondaryAmmoType", |a| {
        let data = weapon_data(a)?;
        Ok(Value::Integer(ammo_id(a.lua, &data.ammo[1])))
    })?;
    def("HasAmmo", |a| {
        let (handle, state) = state(a)?;
        let data = data(a.lua, handle).unwrap_or_default();
        let ids = [ammo_id(a.lua, &data.ammo[0]), ammo_id(a.lua, &data.ammo[1])];
        if ids == [-1, -1] {
            return Ok(Value::Boolean(true));
        }
        let owner = host(a.lua).entities.get(handle).map_or(EntityHandle::NULL, |ent| ent.owner);
        let reserve = |id: i64| crate::player::ammo_count(a.lua, owner, id) > 0;
        Ok(Value::Boolean(state.clip[0] > 0 || state.clip[1] > 0 || reserve(ids[0]) || reserve(ids[1])))
    })?;
    def("GetNextPrimaryFire", |a| Ok(Value::Number(state(a)?.1.next_fire[0])))?;
    def("GetNextSecondaryFire", |a| Ok(Value::Number(state(a)?.1.next_fire[1])))?;
    def("SetNextPrimaryFire", |a| {
        let time = a.number(2)?;
        change(a, |w| w.next_fire[0] = time)
    })?;
    def("SetNextSecondaryFire", |a| {
        let time = a.number(2)?;
        change(a, |w| w.next_fire[1] = time)
    })?;
    def("LastShootTime", |a| Ok(Value::Number(state(a)?.1.last_shoot)))?;
    def("SetLastShootTime", |a| {
        let time = a.opt_number(2)?.unwrap_or_else(|| host(a.lua).clock.cur_time.get());
        change(a, |w| w.last_shoot = time)
    })?;
    def("GetActivity", |a| Ok(Value::Integer(state(a)?.1.activity)))?;
    def("SetActivity", |a| {
        let act = a.number(2)? as i64;
        change(a, |w| w.activity = act)
    })?;
    // the view model of the owner plays a sequence of the activity
    def("SendWeaponAnim", |a| {
        let handle = entity::this(a, 1)?;
        crate::anim::send_weapon_anim(a.lua, handle, a.number(2)? as i64);
        Ok(Value::Nil)
    })?;
    def("GetDeploySpeed", |a| Ok(Value::Number(f64::from(state(a)?.1.deploy_speed))))?;
    def("SetDeploySpeed", |a| {
        let speed = a.number(2)? as f32;
        change(a, |w| w.deploy_speed = speed)
    })?;
    def("GetHoldType", |a| string(a.lua, &state(a)?.1.hold_type))?;
    def("SetHoldType", |a| {
        let handle = entity::this(a, 1)?;
        let name = a.string(2)?;
        change(a, |w| w.hold_type = name.clone())?;
        call(a.lua, handle, "SetWeaponHoldType", MultiValue::from_iter([Value::String(a.lua.create_string(&name)?)]));
        Ok(Value::Nil)
    })?;
    def("GetPrintName", |a| string(a.lua, &weapon_data(a)?.print_name))?;
    def("GetSlot", |a| Ok(Value::Integer(weapon_data(a)?.slot)))?;
    def("GetSlotPos", |a| Ok(Value::Integer(weapon_data(a)?.slot_pos)))?;
    def("GetWeight", |a| Ok(Value::Integer(weapon_data(a)?.weight)))?;
    def("GetWeaponViewModel", |a| string(a.lua, &weapon_data(a)?.view_model))?;
    def("GetWeaponWorldModel", |a| string(a.lua, &weapon_data(a)?.world_model))?;
    def("AllowsAutoSwitchTo", |a| Ok(Value::Boolean(weapon_data(a)?.auto_switch_to)))?;
    def("AllowsAutoSwitchFrom", |a| Ok(Value::Boolean(weapon_data(a)?.auto_switch_from)))?;
    def("IsWeaponVisible", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(host(a.lua).entities.get(handle).is_some_and(|ent| !ent.no_draw && ent.effects & 32 == 0)))
    })?;
    // the server runs the client's part of a SWEP nowhere: there are no clients yet
    def("CallOnClient", |a| {
        entity::this(a, 1)?;
        Ok(Value::Nil)
    })?;
    // starts a reload when the clip is not full and the owner has rounds for it: the view model
    // plays the activity, the owner the reload of their own model, and the clip fills when the
    // view model's sequence has played (the next tick without one)
    def("DefaultReload", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(default_reload(a.lua, handle, a.number(2)? as i64)))
    })?;
    Ok(())
}
