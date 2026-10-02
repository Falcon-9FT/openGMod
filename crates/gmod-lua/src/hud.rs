//! The engine's HUD, as Half-Life 2 draws it and GMod keeps it: health, the suit's armour,
//! the ammo of the weapon in hand - primary and secondary - and the crosshair. Elements go
//! where `scripts/HudLayout.res` puts them, in the coordinates of a screen 480 pixels tall
//! (`r150`: from the right edge, `c-20`: from the centre), with the fonts and colours of
//! `resource/ClientScheme.res`; an installation without those files gets Half-Life 2's.
//!
//! Lua decides what shows: every element asks `HUDShouldDraw` (`CHudHealth`, `CHudBattery`,
//! `CHudAmmo`, `CHudSecondaryAmmo`, `CHudCrosshair`, and `CHudGMod` for Lua's own HUD), a
//! weapon can hide its ammo (`SWEP.DrawAmmo`, `CustomAmmoDisplay`) and its crosshair
//! (`SWEP.DrawCrosshair`, `DoDrawCrosshair`). Around the engine's elements Lua draws its own:
//! `PreDrawHUD`, `HUDPaintBackground`, then the elements, `HUDPaint`, the weapon's `DrawHUD`,
//! `HUDDrawScoreBoard`, `PostDrawHUD`.

use gmod_ui::{FontId, Texture};
use mlua::{IntoLuaMulti, Lua, MultiValue, ObjectLike, Table, Value};

use crate::entity::{self, EntityHandle};
use crate::{call_hook, host};

/// A place along one side of the screen, in units of a screen 480 pixels tall.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Coord {
    /// From the left or top edge.
    Start(f32),
    /// From the right or bottom edge: `r150`.
    End(f32),
    /// From the centre: `c-20`.
    Centre(f32),
}

impl Coord {
    fn parse(text: &str) -> Option<Coord> {
        let text = text.trim();
        let (make, rest): (fn(f32) -> Coord, &str) = match text.as_bytes().first()? {
            b'r' | b'R' => (Coord::End, &text[1..]),
            b'c' | b'C' => (Coord::Centre, &text[1..]),
            _ => (Coord::Start, text),
        };
        rest.trim().parse().ok().map(make)
    }

    /// In pixels, on a side `side` pixels long, `scale` pixels to the unit.
    fn pixels(self, side: f32, scale: f32) -> f32 {
        match self {
            Coord::Start(v) => v * scale,
            Coord::End(v) => side - v * scale,
            Coord::Centre(v) => side / 2.0 + v * scale,
        }
    }
}

/// An element of the HUD as `scripts/HudLayout.res` lays it out: its box, and where its
/// label, its number and its second number go in it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Element {
    x: Coord,
    y: Coord,
    wide: f32,
    tall: f32,
    visible: bool,
    text: [f32; 2],
    digit: [f32; 2],
    digit2: [f32; 2],
}

impl Element {
    const fn new(x: Coord, y: f32, wide: f32, text: [f32; 2], digit: [f32; 2], digit2: [f32; 2]) -> Element {
        Element { x, y: Coord::Start(y), wide, tall: 36.0, visible: true, text, digit, digit2 }
    }

    /// What a block of `HudLayout.res` changes.
    fn read(mut self, block: &gmod_kv::Block) -> Element {
        let number = |key: &str| block.str(key).and_then(|v| v.trim().parse::<f32>().ok());
        if let Some(x) = block.str("xpos").and_then(Coord::parse) {
            self.x = x;
        }
        if let Some(y) = block.str("ypos").and_then(Coord::parse) {
            self.y = y;
        }
        self.wide = number("wide").unwrap_or(self.wide);
        self.tall = number("tall").unwrap_or(self.tall);
        self.visible = number("visible").map_or(self.visible, |v| v != 0.0);
        for (key, field) in [("text", &mut self.text), ("digit", &mut self.digit), ("digit2", &mut self.digit2)] {
            field[0] = number(&format!("{key}_xpos")).unwrap_or(field[0]);
            field[1] = number(&format!("{key}_ypos")).unwrap_or(field[1]);
        }
        self
    }
}

/// Half-Life 2's layout as GMod changed it.
const HEALTH: Element = Element::new(Coord::Start(16.0), 432.0, 102.0, [8.0, 20.0], [50.0, 2.0], [0.0; 2]);
const SUIT: Element = Element::new(Coord::Start(140.0), 432.0, 102.0, [8.0, 20.0], [50.0, 2.0], [0.0; 2]);
const AMMO: Element = Element::new(Coord::End(150.0), 432.0, 136.0, [8.0, 20.0], [40.0, 2.0], [102.0, 16.0]);
const AMMO2: Element = Element::new(Coord::End(82.0), 432.0, 72.0, [8.0, 22.0], [26.0, 2.0], [88.0, 16.0]);

/// The crosshair's picture: part of a texture, as `scripts/hud_textures.txt` cuts it.
#[derive(Debug, Clone, PartialEq)]
struct Icon {
    file: String,
    rect: [f32; 4],
}

/// How the engine's HUD looks, read when the client starts.
pub(crate) struct Hud {
    health: Element,
    suit: Element,
    ammo: Element,
    ammo2: Element,
    fg: [u8; 4],
    bg: [u8; 4],
    damaged_fg: [u8; 4],
    damaged_bg: [u8; 4],
    crosshair: Icon,
}

impl Default for Hud {
    fn default() -> Hud {
        Hud {
            health: HEALTH,
            suit: SUIT,
            ammo: AMMO,
            ammo2: AMMO2,
            fg: [255, 220, 0, 220],
            bg: [0, 0, 0, 90],
            damaged_fg: [180, 0, 0, 230],
            damaged_bg: [180, 0, 0, 200],
            crosshair: Icon { file: "sprites/crosshairs".to_string(), rect: [0.0, 48.0, 24.0, 24.0] },
        }
    }
}

/// Everything after the root key of a key-values file.
fn read_kv(lua: &Lua, path: &str) -> Option<gmod_kv::Block> {
    let data = host(lua).vfs.read(path, "GAME").ok()?;
    let (mut document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&data), gmod_kv::Options::default());
    document.root.apply_conditions(&gmod_kv::pc_symbol);
    document.root.iter().find_map(|p| p.value.as_block()).cloned()
}

/// A colour of a scheme: `"255 220 0 100"`, or the name of another.
fn scheme_color(scheme: &gmod_kv::Block, value: &str, depth: u32) -> Option<[u8; 4]> {
    let parts: Vec<u8> = value.split_whitespace().filter_map(|p| p.parse::<f32>().ok()).map(|n| n.clamp(0.0, 255.0) as u8).collect();
    if parts.len() >= 3 {
        return Some([parts[0], parts[1], parts[2], parts.get(3).copied().unwrap_or(255)]);
    }
    if depth > 8 {
        return None;
    }
    let named = ["Colors", "BaseSettings"].iter().find_map(|b| scheme.block(b).and_then(|b| b.str(value)))?;
    scheme_color(scheme, named, depth + 1)
}

/// Reads the layout, the colours and the crosshair of the installation.
pub(crate) fn load(lua: &Lua) {
    let mut hud = Hud::default();
    if let Some(layout) = read_kv(lua, "scripts/HudLayout.res") {
        for (name, element) in
            [("HudHealth", &mut hud.health), ("HudSuit", &mut hud.suit), ("HudAmmo", &mut hud.ammo), ("HudAmmoSecondary", &mut hud.ammo2)]
        {
            if let Some(block) = layout.block(name) {
                *element = element.read(block);
            }
        }
    }
    if let Some(scheme) = read_kv(lua, "resource/ClientScheme.res") {
        let color = |names: &[&str]| {
            names.iter().find_map(|name| scheme.block("BaseSettings")?.str(name).and_then(|v| scheme_color(&scheme, v, 0)))
        };
        hud.fg = color(&["FgColorHud", "FgColor"]).unwrap_or(hud.fg);
        hud.bg = color(&["BgColorHud", "BgColor"]).unwrap_or(hud.bg);
        hud.damaged_fg = color(&["DamagedFg"]).unwrap_or(hud.damaged_fg);
        hud.damaged_bg = color(&["DamagedBg"]).unwrap_or(hud.damaged_bg);
    }
    if let Some(icon) = read_kv(lua, "scripts/hud_textures.txt").as_ref().and_then(|t| t.block("TextureData")?.block("crosshair_default"))
        && let Some(file) = icon.str("file")
    {
        let number = |key: &str| icon.str(key).and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(0.0);
        hud.crosshair = Icon { file: file.to_string(), rect: [number("x"), number("y"), number("width"), number("height")] };
    }
    *host(lua).hud.borrow_mut() = hud;
}

/// Whether Lua lets an element of the HUD draw (`HUDShouldDraw`).
fn should_draw(lua: &Lua, name: &str) -> bool {
    let args = name.into_lua_multi(lua).unwrap_or_default();
    !matches!(call_hook(lua, "HUDShouldDraw", args).front(), Some(Value::Boolean(false)))
}

/// A field of a scripted weapon's table (`SWEP.DrawAmmo`), if it is one.
fn swep_field(lua: &Lua, weapon: EntityHandle, name: &str) -> Value {
    if !host(lua).entities.get(weapon).is_some_and(|ent| ent.scripted) {
        return Value::Nil;
    }
    match entity::push(lua, weapon) {
        Ok(Value::UserData(ud)) => ud.get::<Value>(name).unwrap_or(Value::Nil),
        _ => Value::Nil,
    }
}

/// What the ammo elements show: the big number and the small one beside it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ammo {
    primary: Option<(i64, Option<i64>)>,
    secondary: Option<i64>,
}

fn ammo(lua: &Lua, player: EntityHandle, weapon: EntityHandle) -> Ammo {
    let none = Ammo { primary: None, secondary: None };
    if matches!(swep_field(lua, weapon, "DrawAmmo"), Value::Boolean(false)) {
        return none;
    }
    let number = |t: &Table, key: &str| t.get::<Option<f64>>(key).ok().flatten().map(|n| n as i64);
    if let Some(Value::Table(custom)) =
        crate::weapon::call(lua, weapon, "CustomAmmoDisplay", MultiValue::new()).and_then(|r| r.into_iter().next())
    {
        if !custom.get::<Option<bool>>("Draw").ok().flatten().unwrap_or(false) {
            return none;
        }
        let primary = match (number(&custom, "PrimaryClip"), number(&custom, "PrimaryAmmo")) {
            (Some(clip), ammo) if clip >= 0 => Some((clip, ammo.filter(|a| *a >= 0))),
            (_, Some(ammo)) if ammo >= 0 => Some((ammo, None)),
            _ => None,
        };
        return Ammo { primary, secondary: number(&custom, "SecondaryAmmo").filter(|a| *a >= 0) };
    }
    let Some(data) = crate::weapon::data(lua, weapon) else { return none };
    let clip = host(lua).entities.get(weapon).and_then(|ent| ent.weapon.as_ref().map(|w| w.clip[0])).unwrap_or(-1);
    let carried = |i: usize| crate::player::ammo_count(lua, player, crate::weapon::ammo_id(lua, &data.ammo[i]));
    let primary = match data.ammo[0].is_empty() {
        true => None,
        false if clip < 0 => Some((carried(0), None)),
        false => Some((clip, Some(carried(0)))),
    };
    let secondary = (!data.ammo[1].is_empty()).then(|| carried(1));
    Ammo { primary, secondary }
}

/// The phrase of a label, or the English when the game's language files do not have it.
fn label(lua: &Lua, token: &str, english: &str) -> String {
    let phrase = host(lua).client.phrase(token);
    if phrase == token { english.to_string() } else { phrase }
}

/// A box with round corners, as the HUD's backgrounds are.
fn rounded_box(lua: &Lua, rect: [f32; 4], radius: f32, color: [u8; 4]) {
    let [x, y, w, h] = rect;
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut points = Vec::with_capacity(4 * 5);
    let corners = [(x + w - r, y + r, -90.0f32), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, 90.0), (x + r, y + r, 180.0)];
    for (cx, cy, start) in corners {
        for step in 0..=4 {
            let angle = (start + step as f32 * 22.5).to_radians();
            points.push([cx + r * angle.cos(), cy + r * angle.sin(), 0.0, 0.0]);
        }
    }
    let screen = &host(lua).screen;
    let color = screen.color(color);
    screen.canvas.borrow_mut().polygon(Texture::White, false, &points, color);
}

/// Draws an element with numbers: its box, its label, its number, and the small number.
fn numeric(lua: &Lua, element: &Element, text: &str, value: i64, small: Option<i64>, fg: [u8; 4], bg: [u8; 4]) {
    if !element.visible {
        return;
    }
    let screen = &host(lua).screen;
    let (w, h) = screen.size.get();
    let scale = h as f32 / 480.0;
    let x = element.x.pixels(w as f32, scale).round();
    let y = element.y.pixels(h as f32, scale).round();
    rounded_box(lua, [x, y, (element.wide * scale).round(), (element.tall * scale).round()], 8.0, bg);
    let at = |p: [f32; 2]| (x + p[0] * scale, y + p[1] * scale);
    let font = |names: &[&str]| -> Option<FontId> { names.iter().find_map(|n| screen.proportional_font(n)) };
    if let Some(font) = font(&["HudDefault", "Default"]) {
        let (tx, ty) = at(element.text);
        screen.text(font, tx, ty, text, fg);
    }
    if let Some(font) = font(&["HudNumbers"]) {
        let (dx, dy) = at(element.digit);
        screen.text(font, dx, dy, &value.to_string(), fg);
    }
    if let (Some(small), Some(font)) = (small, font(&["HudNumbersSmall"])) {
        let (dx, dy) = at(element.digit2);
        screen.text(font, dx, dy, &small.to_string(), fg);
    }
}

/// The crosshair in the middle of the screen, unless the weapon draws its own.
fn crosshair(lua: &Lua, weapon: Option<EntityHandle>) {
    if crate::convar::value(lua, "crosshair").is_some_and(|v| crate::convar::atof(&v) == 0.0) || !should_draw(lua, "CHudCrosshair") {
        return;
    }
    let (w, h) = host(lua).screen.size.get();
    let (cx, cy) = ((w / 2) as f32, (h / 2) as f32);
    if let Some(weapon) = weapon {
        if matches!(swep_field(lua, weapon, "DrawCrosshair"), Value::Boolean(false)) {
            return;
        }
        let args = (cx, cy).into_lua_multi(lua).unwrap_or_default();
        let custom = crate::weapon::call(lua, weapon, "DoDrawCrosshair", args);
        if matches!(custom.as_ref().and_then(|r| r.front()), Some(Value::Boolean(true))) {
            return;
        }
    }
    let host = host(lua);
    let icon = host.hud.borrow().crosshair.clone();
    let screen = &host.screen;
    let id = screen.image_id(&icon.file, true);
    let color = screen.color([255, 255, 255, 255]);
    let [ix, iy, iw, ih] = icon.rect;
    match crate::surface::image_size_of(lua, id) {
        Some((tw, th)) if iw > 0.0 && ih > 0.0 => {
            let (tw, th) = (tw as f32, th as f32);
            let rect = [(cx - iw / 2.0).round(), (cy - ih / 2.0).round(), iw, ih];
            let uv = [ix / tw, iy / th, (ix + iw) / tw, (iy + ih) / th];
            screen.canvas.borrow_mut().image(Texture::Image(id), true, rect, uv, color);
        }
        // no texture: four short lines round the middle
        _ => {
            let mut canvas = screen.canvas.borrow_mut();
            for (dx, dy, lw, lh) in [(-9.0, -0.5, 6.0, 1.0), (3.0, -0.5, 6.0, 1.0), (-0.5, -9.0, 1.0, 6.0), (-0.5, 3.0, 1.0, 6.0)] {
                canvas.rect(cx + dx, cy + dy, lw, lh, color);
            }
        }
    }
}

/// Draws the HUD: Lua's around the engine's. Nothing with `cl_drawhud 0`.
pub(crate) fn paint(lua: &Lua) {
    if crate::convar::value(lua, "cl_drawhud").is_some_and(|v| crate::convar::atof(&v) == 0.0) {
        return;
    }
    call_hook(lua, "PreDrawHUD", MultiValue::new());
    call_hook(lua, "HUDPaintBackground", MultiValue::new());
    let player = host(lua).client.local_player.get();
    let status = crate::player::state(lua, player, |ent, p| (p.alive, ent.health, p.armor, p.active_weapon));
    let weapon = status.and_then(|(alive, _, _, weapon)| (alive && host(lua).entities.is_valid(weapon)).then_some(weapon));
    if let Some((true, health, armor, _)) = status {
        let (health_element, suit, ammo_element, ammo2, fg, bg, damaged_fg, damaged_bg) = {
            let host = host(lua);
            let hud = host.hud.borrow();
            (hud.health, hud.suit, hud.ammo, hud.ammo2, hud.fg, hud.bg, hud.damaged_fg, hud.damaged_bg)
        };
        if should_draw(lua, "CHudHealth") {
            let (hfg, hbg) = if health < 20 { (damaged_fg, damaged_bg) } else { (fg, bg) };
            numeric(lua, &health_element, &label(lua, "#Valve_Hud_HEALTH", "HEALTH"), health.max(0), None, hfg, hbg);
        }
        if armor > 0 && should_draw(lua, "CHudBattery") {
            numeric(lua, &suit, &label(lua, "#Valve_Hud_SUIT", "SUIT"), armor, None, fg, bg);
        }
        if let Some(weapon) = weapon {
            let shown = ammo(lua, player, weapon);
            if let Some((value, small)) = shown.primary
                && should_draw(lua, "CHudAmmo")
            {
                numeric(lua, &ammo_element, &label(lua, "#Valve_Hud_AMMO", "AMMO"), value, small, fg, bg);
            }
            if let Some(value) = shown.secondary
                && should_draw(lua, "CHudSecondaryAmmo")
            {
                numeric(lua, &ammo2, &label(lua, "#Valve_Hud_AMMO_ALT", "ALT"), value, None, fg, bg);
            }
        }
        crosshair(lua, weapon);
    }
    if should_draw(lua, "CHudGMod") {
        call_hook(lua, "HUDPaint", MultiValue::new());
        if let Some(weapon) = weapon {
            crate::weapon::call(lua, weapon, "DrawHUD", MultiValue::new());
        }
        call_hook(lua, "HUDDrawScoreBoard", MultiValue::new());
    }
    call_hook(lua, "PostDrawHUD", MultiValue::new());
}
