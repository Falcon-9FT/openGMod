//! Choosing a weapon as Half-Life 2's HUD does it (`CHudWeaponSelection`): the mouse wheel and
//! the number keys (`invnext`, `invprev`, `slot1` to `slot6`) open a row of the weapon slots at
//! the top of the screen and move the choice - the wheel through all the weapons, a number
//! key through those of its slot - and the attack key takes the weapon chosen (`use <class>`,
//! for the server), the second attack key or a few seconds without a key put the row away.
//!
//! The slot chosen shows its weapons one under another, the chosen one bigger and with its
//! name; the others show their number. A scripted weapon draws itself in its box
//! (`SWEP:DrawWeaponSelection`), one of the engine's draws its icon from its script's font
//! (`WeaponIcons`). Lua decides first: `PlayerBindPress` may take the key, and a HUD that hides
//! the row (`HUDShouldDraw("CHudWeaponSelection")`) gets the keys as they are, to switch at
//! once.

use std::cell::Cell;

use mlua::{IntoLuaMulti, Lua, MultiValue, Value};

use crate::entity::{self, EntityHandle};
use crate::host;

/// Seconds the row stays without a key, and how long it then takes to fade.
const SHOWN_FOR: f64 = 2.5;
const FADE: f64 = 0.5;
/// Slots across the row.
const SLOTS: i64 = 6;

/// What the keys of choosing do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choose {
    Next,
    Previous,
    /// A slot, counted from 0.
    Slot(i64),
}

impl Choose {
    /// The console command of the key.
    fn bind(self) -> String {
        match self {
            Choose::Next => "invnext".into(),
            Choose::Previous => "invprev".into(),
            Choose::Slot(slot) => format!("slot{}", slot + 1),
        }
    }
}

/// The row of slots: whether it shows, the weapon chosen, and when a key last moved it.
#[derive(Default)]
pub(crate) struct Selection {
    open: Cell<bool>,
    chosen: Cell<EntityHandle>,
    moved: Cell<f64>,
}

fn now(lua: &Lua) -> f64 {
    host(lua).started.elapsed().as_secs_f64()
}

/// The local player's weapons in the order of the row: by slot, by place in it.
fn weapons(lua: &Lua) -> Vec<(EntityHandle, i64, i64)> {
    let player = host(lua).client.local_player.get();
    let owned = crate::player::state(lua, player, |_, p| p.weapons.clone()).unwrap_or_default();
    let mut list: Vec<(EntityHandle, i64, i64)> = owned
        .into_iter()
        .filter(|&w| host(lua).entities.is_valid(w))
        .filter_map(|w| crate::weapon::data(lua, w).map(|d| (w, d.slot.clamp(0, SLOTS - 1), d.slot_pos)))
        .collect();
    list.sort_by_key(|&(w, slot, pos)| (slot, pos, w.index));
    list
}

fn active(lua: &Lua) -> EntityHandle {
    let player = host(lua).client.local_player.get();
    crate::player::active_weapon(lua, player).unwrap_or(EntityHandle::NULL)
}

fn should_draw(lua: &Lua) -> bool {
    let args = "CHudWeaponSelection".into_lua_multi(lua).unwrap_or_default();
    !matches!(crate::call_hook(lua, "HUDShouldDraw", args).front(), Some(Value::Boolean(false)))
}

/// `PlayerBindPress(ply, bind, true)`: whether Lua took the key.
fn taken_by_lua(lua: &Lua, bind: &str) -> bool {
    let player = host(lua).client.local_player.get();
    let (Ok(ply), Ok(bind)) = (entity::push(lua, player), lua.create_string(bind)) else { return false };
    let said = crate::call_hook(lua, "PlayerBindPress", MultiValue::from_iter([ply, Value::String(bind), Value::Boolean(true)]));
    matches!(said.front(), Some(Value::Boolean(true)))
}

/// A key of choosing: the row opens or the choice moves. What to send the server instead,
/// when the HUD does not choose (the key's command, to switch at once).
pub(crate) fn key(lua: &Lua, choose: Choose) -> Option<String> {
    let bind = choose.bind();
    if taken_by_lua(lua, &bind) {
        return None;
    }
    if !should_draw(lua) {
        return Some(bind);
    }
    let list = weapons(lua);
    if list.is_empty() {
        return None;
    }
    let selection = &host(lua).selection;
    let from =
        if selection.open.get() && list.iter().any(|&(w, ..)| w == selection.chosen.get()) { selection.chosen.get() } else { active(lua) };
    let at = list.iter().position(|&(w, ..)| w == from);
    let next = match choose {
        Choose::Next => at.map_or(0, |i| (i + 1) % list.len()),
        Choose::Previous => at.map_or(list.len() - 1, |i| (i + list.len() - 1) % list.len()),
        Choose::Slot(slot) => {
            let in_slot: Vec<usize> = (0..list.len()).filter(|&i| list[i].1 == slot).collect();
            if in_slot.is_empty() {
                return None;
            }
            // again in the same slot: the next one there
            let same = selection.open.get() && at.is_some_and(|i| list[i].1 == slot);
            match (same, at.and_then(|i| in_slot.iter().position(|&j| j == i))) {
                (true, Some(k)) => in_slot[(k + 1) % in_slot.len()],
                _ => in_slot[0],
            }
        }
    };
    selection.chosen.set(list[next].0);
    selection.open.set(true);
    selection.moved.set(now(lua));
    None
}

/// Whether the row shows: the attack keys choose and put it away rather than fire.
pub(crate) fn is_open(lua: &Lua) -> bool {
    let selection = &host(lua).selection;
    if selection.open.get() && now(lua) - selection.moved.get() > SHOWN_FOR + FADE {
        selection.open.set(false);
    }
    selection.open.get()
}

/// The attack key while the row shows: it goes away, and the command that takes the weapon
/// chosen, if it is still there and not already in hand.
pub(crate) fn take(lua: &Lua) -> Option<String> {
    if !is_open(lua) {
        return None;
    }
    let selection = &host(lua).selection;
    selection.open.set(false);
    let chosen = selection.chosen.get();
    if chosen == active(lua) || !host(lua).entities.is_valid(chosen) {
        return None;
    }
    let class = host(lua).entities.get(chosen)?.class.clone();
    Some(format!("use {class}"))
}

/// The row goes away without a weapon taken.
pub(crate) fn cancel(lua: &Lua) {
    host(lua).selection.open.set(false);
}

/// Draws the row, while it shows.
pub(crate) fn paint(lua: &Lua) {
    if !is_open(lua) || !should_draw(lua) {
        return;
    }
    let list = weapons(lua);
    let chosen = host(lua).selection.chosen.get();
    let Some(&(_, chosen_slot, _)) = list.iter().find(|&&(w, ..)| w == chosen) else { return };
    let since = now(lua) - host(lua).selection.moved.get();
    let alpha = if since > SHOWN_FOR { (1.0 - (since - SHOWN_FOR) / FADE).clamp(0.0, 1.0) } else { 1.0 } as f32;
    let screen = &host(lua).screen;
    let (w, h) = screen.size.get();
    let scale = h as f32 / 480.0;
    let (fg, bg) = crate::hud::colors(lua);
    let fade = |c: [u8; 4], a: f32| [c[0], c[1], c[2], (f32::from(c[3]) * a * alpha).round() as u8];
    // Half-Life 2's sizes (`HudWeaponSelection` of `scripts/HudLayout.res`)
    let (small, medium, large, gap) = (32.0 * scale, [95.0 * scale, 50.0 * scale], [112.0 * scale, 80.0 * scale], 8.0 * scale);
    let used: Vec<bool> = (0..SLOTS).map(|s| list.iter().any(|&(_, slot, _)| slot == s)).collect();
    let width: f32 = (0..SLOTS).map(|s| if s == chosen_slot { large[0] } else { small }).sum::<f32>() + gap * (SLOTS - 1) as f32;
    let mut x = ((w as f32 - width) / 2.0).round();
    let top = (16.0 * scale).round();
    let numbers = screen.proportional_font("HudSelectionNumbers");
    let names = screen.proportional_font("HudSelectionText");
    for slot in 0..SLOTS {
        if slot != chosen_slot {
            let shown = if used[slot as usize] { 1.0 } else { 0.5 };
            crate::hud::rounded_box(lua, [x, top, small, small], 6.0, fade(bg, shown));
            if let Some(font) = numbers {
                screen.text(font, x + 4.0 * scale, top + 4.0 * scale, &(slot + 1).to_string(), fade(fg, shown));
            }
            x += small + gap;
            continue;
        }
        let mut y = top;
        for &(weapon, _, _) in list.iter().filter(|&&(_, s, _)| s == slot) {
            let picked = weapon == chosen;
            let [bw, bh] = if picked { large } else { medium };
            let bx = x + (large[0] - bw) / 2.0;
            let box_bg = if picked { [bg[0], bg[1], bg[2], bg[3].saturating_add(60)] } else { bg };
            crate::hud::rounded_box(lua, [bx, y, bw, bh], 6.0, fade(box_bg, 1.0));
            draw_weapon(lua, weapon, [bx, y, bw, bh], picked, fade(fg, if picked { 1.0 } else { 0.7 }), alpha);
            if picked && let Some(font) = names {
                let name = crate::weapon::data(lua, weapon).map(|d| d.print_name).unwrap_or_default();
                let name = host(lua).client.phrase(&name);
                let (tw, _) = screen.fonts.borrow_mut().text_size(font, &name);
                screen.text(font, bx + (bw - tw as f32) / 2.0, y + bh - 18.0 * scale, &name, fade(fg, 1.0));
            }
            if let Some(font) = numbers.filter(|_| y == top) {
                screen.text(font, bx + 4.0 * scale, y + 4.0 * scale, &(slot + 1).to_string(), fade(fg, 1.0));
            }
            y += bh + gap;
        }
        x += large[0] + gap;
    }
}

/// A weapon in its box: a scripted one draws itself, one of the engine's shows its icon.
fn draw_weapon(lua: &Lua, weapon: EntityHandle, rect: [f32; 4], picked: bool, color: [u8; 4], alpha: f32) {
    let [x, y, w, h] = rect;
    let scripted = host(lua).entities.get(weapon).is_some_and(|e| e.scripted);
    if scripted {
        let args = MultiValue::from_iter([x, y, w, h, 255.0 * alpha].map(|v| Value::Number(f64::from(v))));
        crate::weapon::call(lua, weapon, "DrawWeaponSelection", args);
        return;
    }
    let Some(data) = crate::weapon::data(lua, weapon) else { return };
    let icon = if picked { data.icons[1].clone().or_else(|| data.icons[0].clone()) } else { data.icons[0].clone() };
    let screen = &host(lua).screen;
    match icon.and_then(|(font, character)| Some((screen.proportional_font(&font)?, character))) {
        Some((font, character)) => {
            let (tw, th) = screen.fonts.borrow_mut().text_size(font, &character);
            screen.text(font, x + (w - tw as f32) / 2.0, y + (h - th as f32) / 2.0, &character, color);
        }
        // no icon: its name
        None => {
            let Some(font) = screen.proportional_font("HudSelectionText") else { return };
            let name = host(lua).client.phrase(&data.print_name);
            let (tw, th) = screen.fonts.borrow_mut().text_size(font, &name);
            screen.text(font, x + (w - tw as f32) / 2.0, y + (h - th as f32) / 2.0, &name, color);
        }
    }
}
