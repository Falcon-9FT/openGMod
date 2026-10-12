//! Half-Life 2's items, as GMod has them: what the spawn menu's Entities tab and the maps put
//! down for players to take.
//!
//! * A kit, a battery or a box of rounds (`item_healthkit`, `item_battery`, `item_ammo_smg1`
//!   ...) takes its model when it spawns, and a body of it: it falls and rolls. A player alive
//!   who walks over it takes it when it is of any use - health below their most, armor below
//!   theirs (100), rounds of its type below what they may carry - and the gamemode's
//!   `PlayerCanPickupItem` does not say no: the health, armor or rounds (as many as they may
//!   carry), the item's sound, its `OnPlayerTouch`, and it is gone. A full player leaves it.
//!   What each gives is HL2's default: a kit 25 health, a vial 10, a battery 15 armor; boxes
//!   20 pistol rounds (100 the large), 45 SMG rounds (225), 20 AR2 (100), 6 .357 (20),
//!   6 bolts, 1 rocket, 1 SMG grenade, 20 shells, 1 combine ball.
//! * The chargers on walls (`item_healthcharger`, `item_suitcharger`): while a player holds
//!   the use key on one, it gives them a point of health or armor every tenth of a second
//!   from what it holds (50 health, 75 armor) - `OnPlayerUse` and its sound when they start,
//!   `OutRemainingHealth` / `OutRemainingCharge` with what is left (0 to 1), `OnEmpty` when
//!   nothing is; a player it cannot help, or an empty charger, hears the denial. `Recharge`
//!   fills it again.
//!
//! Not done: the client's notices of what was picked up (`HUDItemPickedUp`,
//! `HUDAmmoPickedUp`), the crates of ammo and items, chargers filling again by themselves,
//! the Citadel's suit charger, the charging loop sound.

use mlua::{Lua, MultiValue, Value};

use crate::entity::{self, EntityHandle, Kind};
use crate::host;
use crate::vector::Vector;

/// What an item gives.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Gives {
    Health(i64),
    Armor(i64),
    /// Rounds of an ammo type, by name.
    Ammo(&'static str, i64),
}

/// The items: class, model, what it gives, the sound of taking it.
const ITEMS: &[(&str, &str, Gives, &str)] = &[
    ("item_healthkit", "models/items/healthkit.mdl", Gives::Health(25), "HealthKit.Touch"),
    ("item_healthvial", "models/healthvial.mdl", Gives::Health(10), "HealthVial.Touch"),
    ("item_battery", "models/items/battery.mdl", Gives::Armor(15), "ItemBattery.Touch"),
    ("item_ammo_pistol", "models/items/boxsrounds.mdl", Gives::Ammo("Pistol", 20), AMMO),
    ("item_ammo_pistol_large", "models/items/largeboxsrounds.mdl", Gives::Ammo("Pistol", 100), AMMO),
    ("item_ammo_smg1", "models/items/boxmrounds.mdl", Gives::Ammo("SMG1", 45), AMMO),
    ("item_ammo_smg1_large", "models/items/largeboxmrounds.mdl", Gives::Ammo("SMG1", 225), AMMO),
    ("item_ammo_ar2", "models/items/combine_rifle_cartridge01.mdl", Gives::Ammo("AR2", 20), AMMO),
    ("item_ammo_ar2_large", "models/items/combine_rifle_cartridge01.mdl", Gives::Ammo("AR2", 100), AMMO),
    ("item_ammo_357", "models/items/357ammo.mdl", Gives::Ammo("357", 6), AMMO),
    ("item_ammo_357_large", "models/items/357ammobox.mdl", Gives::Ammo("357", 20), AMMO),
    ("item_ammo_crossbow", "models/items/crossbowrounds.mdl", Gives::Ammo("XBowBolt", 6), AMMO),
    ("item_rpg_round", "models/weapons/w_missile_closed.mdl", Gives::Ammo("RPG_Round", 1), AMMO),
    ("item_ammo_smg1_grenade", "models/items/ar2_grenade.mdl", Gives::Ammo("SMG1_Grenade", 1), AMMO),
    ("item_box_buckshot", "models/items/boxbuckshot.mdl", Gives::Ammo("Buckshot", 20), AMMO),
    ("item_ammo_ar2_altfire", "models/items/combine_rifle_ammo01.mdl", Gives::Ammo("AR2AltFire", 1), AMMO),
];

const AMMO: &str = "BaseCombatCharacter.AmmoPickup";

/// How near a player's box must come to an item's to take it.
const REACH: f32 = 8.0;

const SOLID_BBOX: i64 = 2;
const SOLID_VPHYSICS: i64 = 6;
const MOVETYPE_VPHYSICS: i64 = 6;

fn item(class: &str) -> Option<(&'static str, Gives, &'static str)> {
    ITEMS.iter().find(|(c, ..)| c.eq_ignore_ascii_case(class)).map(|&(_, model, gives, sound)| (model, gives, sound))
}

/// The model an item has when nothing gives it another.
#[cfg(test)]
pub(crate) fn model_of(class: &str) -> Option<&'static str> {
    item(class).map(|(model, ..)| model).or_else(|| charger_kind(class).map(|k| k.model()))
}

fn class_of(lua: &Lua, handle: EntityHandle) -> String {
    host(lua).entities.get(handle).filter(|ent| !ent.scripted).map(|ent| ent.class.clone()).unwrap_or_default()
}

/// A spawned item takes its model and its body; a charger its model and what it holds.
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    let class = class_of(lua, handle);
    let (model, charger) = match (item(&class), charger_kind(&class)) {
        (Some((model, ..)), _) => (model, None),
        (None, Some(kind)) => (kind.model(), Some(kind)),
        (None, None) => return,
    };
    if host(lua).entities.get(handle).is_some_and(|ent| ent.model.is_empty()) {
        entity::set_model(lua, handle, model);
    }
    if let Some(kind) = charger {
        host(lua).entities.with_mut(handle, |ent| {
            ent.solid = SOLID_BBOX;
            ent.charger = Some(Box::new(Charger { kind, juice: kind.full(), next: 0.0, last_use: f64::NEG_INFINITY }));
        });
        return;
    }
    host(lua).entities.with_mut(handle, |ent| {
        ent.solid = SOLID_VPHYSICS;
        ent.move_type = MOVETYPE_VPHYSICS;
    });
    // without a physics model it stays where it was put, a box to walk over
    if !crate::physics::init_model(lua, handle) {
        host(lua).entities.with_mut(handle, |ent| ent.solid = SOLID_BBOX);
    }
}

fn overlap(a: (Vector, Vector), b: (Vector, Vector)) -> bool {
    a.0.x <= b.1.x && a.1.x >= b.0.x && a.0.y <= b.1.y && a.1.y >= b.0.y && a.0.z <= b.1.z && a.1.z >= b.0.z
}

/// One tick of players walking over items: each player alive by their box.
pub(crate) fn touch(lua: &Lua, players: &[(EntityHandle, Vector, Vector)]) {
    if players.is_empty() {
        return;
    }
    let items: Vec<(EntityHandle, Vector, Vector, Gives, &'static str)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                if ent.removed || !ent.spawned || ent.scripted || ent.parent.index.is_some() {
                    return None;
                }
                let (_, gives, sound) = item(&ent.class)?;
                drop(ent);
                let (_, _, lo, hi) = entities.world_bounds(h)?;
                Some((h, lo, hi, gives, sound))
            })
            .collect()
    };
    let reach = Vector::new(REACH, REACH, REACH);
    for &(player, lo, hi) in players {
        for &(it, item_lo, item_hi, gives, sound) in &items {
            if overlap((lo, hi), (item_lo - reach, item_hi + reach)) && host(lua).entities.is_valid(it) {
                take(lua, player, it, gives, sound);
            }
        }
    }
}

/// A player takes an item, if it is of use to them and the gamemode lets them.
fn take(lua: &Lua, player: EntityHandle, it: EntityHandle, gives: Gives, sound: &str) {
    let ammo = |name: &str| crate::weapon::ammo_id(lua, name);
    let useful = match gives {
        Gives::Health(_) => host(lua).entities.get(player).is_some_and(|ent| ent.health < ent.max_health),
        Gives::Armor(_) => crate::player::state(lua, player, |_, p| p.armor < p.max_armor).unwrap_or(false),
        Gives::Ammo(name, _) => {
            let id = ammo(name);
            id > 0 && crate::player::ammo_count(lua, player, id) < crate::weapon::ammo_max(lua, id)
        }
    };
    if !useful {
        return;
    }
    let push = |h: EntityHandle| entity::push(lua, h).unwrap_or(Value::Nil);
    let allowed = crate::call_hook(lua, "PlayerCanPickupItem", MultiValue::from_iter([push(player), push(it)]));
    if matches!(allowed.front(), Some(Value::Boolean(false))) || !host(lua).entities.is_valid(it) {
        return;
    }
    match gives {
        Gives::Health(amount) => {
            host(lua).entities.with_mut(player, |ent| ent.health = (ent.health + amount).min(ent.max_health).max(ent.health));
        }
        Gives::Armor(amount) => {
            crate::player::with_player(lua, player, |_, p| p.armor = (p.armor + amount).min(p.max_armor).max(p.armor));
        }
        Gives::Ammo(name, amount) => {
            crate::player::give_ammo(lua, player, ammo(name), amount);
        }
    }
    crate::sound::emit_scaled(lua, sound, player, 1.0);
    crate::outputs::fire(lua, it, "OnPlayerTouch", player, it, None);
    crate::ents::remove(lua, it);
}

/// Which charger a class is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChargerKind {
    Health,
    Suit,
}

impl ChargerKind {
    fn model(self) -> &'static str {
        match self {
            ChargerKind::Health => "models/props_combine/health_charger001.mdl",
            ChargerKind::Suit => "models/props_combine/suit_charger001.mdl",
        }
    }

    /// What a full one holds: HL2's defaults.
    fn full(self) -> i64 {
        match self {
            ChargerKind::Health => 50,
            ChargerKind::Suit => 75,
        }
    }

    fn sounds(self) -> (&'static str, &'static str) {
        match self {
            ChargerKind::Health => ("WallHealth.Start", "WallHealth.Deny"),
            ChargerKind::Suit => ("SuitRecharge.Start", "SuitRecharge.Deny"),
        }
    }
}

fn charger_kind(class: &str) -> Option<ChargerKind> {
    match class.to_ascii_lowercase().as_str() {
        "item_healthcharger" => Some(ChargerKind::Health),
        "item_suitcharger" => Some(ChargerKind::Suit),
        _ => None,
    }
}

/// What a charger keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct Charger {
    kind: ChargerKind,
    /// What it has left to give.
    juice: i64,
    /// When it gives the next point.
    next: f64,
    /// When it was last used: a use after a pause is a new one.
    last_use: f64,
}

/// How often a charger gives a point while it is used, and how long a pause in using it
/// makes the next use a new one.
const CHARGE_EVERY: f64 = 0.1;
const PAUSE: f64 = 0.25;

/// Whether an entity is a charger: the use key held on it uses it every tick.
pub(crate) fn is_charger(lua: &Lua, handle: EntityHandle) -> bool {
    host(lua).entities.get(handle).is_some_and(|ent| ent.charger.is_some())
}

/// An input to a charger (`Use`, `Recharge`). Whether it took it.
pub(crate) fn input(lua: &Lua, handle: EntityHandle, input: &str, activator: EntityHandle) -> bool {
    let Some(mut c) = host(lua).entities.get(handle).and_then(|ent| ent.charger.as_deref().cloned()) else { return false };
    match input.to_ascii_lowercase().as_str() {
        "use" => charge(lua, handle, &mut c, activator),
        "recharge" => c.juice = c.kind.full(),
        _ => return false,
    }
    host(lua).entities.with_mut(handle, |ent| ent.charger = Some(Box::new(c)));
    true
}

/// A charger used by a player, this tick.
fn charge(lua: &Lua, handle: EntityHandle, c: &mut Charger, player: EntityHandle) {
    if player.kind != Kind::Player {
        return;
    }
    let now = host(lua).clock.cur_time.get();
    let started = now - c.last_use > PAUSE;
    c.last_use = now;
    let (start, deny) = c.kind.sounds();
    let full = match c.kind {
        ChargerKind::Health => host(lua).entities.get(player).is_none_or(|ent| ent.health >= ent.max_health),
        ChargerKind::Suit => crate::player::state(lua, player, |_, p| p.armor >= p.max_armor).unwrap_or(true),
    };
    if c.juice <= 0 || full {
        if started {
            crate::sound::emit_scaled(lua, deny, handle, 1.0);
        }
        return;
    }
    if started {
        crate::outputs::fire(lua, handle, "OnPlayerUse", player, handle, None);
        crate::sound::emit_scaled(lua, start, handle, 1.0);
    }
    if now < c.next {
        return;
    }
    c.next = now + CHARGE_EVERY;
    c.juice -= 1;
    match c.kind {
        ChargerKind::Health => {
            host(lua).entities.with_mut(player, |ent| ent.health += 1);
        }
        ChargerKind::Suit => {
            crate::player::with_player(lua, player, |_, p| p.armor += 1);
        }
    }
    let left = c.juice as f64 / c.kind.full() as f64;
    let output = if c.kind == ChargerKind::Health { "OutRemainingHealth" } else { "OutRemainingCharge" };
    crate::outputs::fire(lua, handle, output, player, handle, Some(&left.to_string()));
    if c.juice == 0 {
        crate::outputs::fire(lua, handle, "OnEmpty", player, handle, None);
    }
}
