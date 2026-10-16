//! What only a client has: the player who plays it (`LocalPlayer`) and where they look,
//! the keys and the mouse (`input`, `gui`), key bindings, the phrases of the game's language
//! (`language`), and the libraries of drawing in 3D that the HUD and the panels touch
//! (`render`, `cam`) - which draw nothing here yet and answer as a plain view would.
//!
//! The program tells the client what happens: the screen's size, keys and buttons going down
//! and up, the cursor, the view of the local player ([`crate::Runtime::set_view`]).
//! Commands the client does not know go to the server ([`crate::Runtime::take_server_commands`]),
//! as a client's console sends them, and so does its user info ([`crate::Runtime::take_user_info`]).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::host;
use crate::vector::{Angle, Vector};

/// Where the local player sees from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub origin: Vector,
    pub angles: Angle,
    /// Horizontal field of view in degrees, as for a 4:3 screen (the engine's way).
    pub fov: f32,
}

impl Default for View {
    fn default() -> View {
        View { origin: Vector::default(), angles: Angle::default(), fov: 75.0 }
    }
}

#[derive(Default)]
struct Keys {
    down: HashSet<i32>,
    /// Went down or up since the last frame.
    pressed: HashSet<i32>,
    released: HashSet<i32>,
    trapping: bool,
    trapped: Option<i32>,
}

pub(crate) struct Client {
    pub local_player: Cell<EntityHandle>,
    /// The map is loaded and its entities have not come yet: `InitPostEntity` waits for them.
    pub awaiting_entities: Cell<bool>,
    pub view: Cell<View>,
    keys: RefCell<Keys>,
    pub cursor: Cell<(i32, i32)>,
    /// `gui.EnableScreenClicker`: the cursor is free over the game.
    pub screen_clicker: Cell<bool>,
    /// Key name (lower case) to command, as `cfg/config_default.cfg` binds them.
    pub(crate) binds: RefCell<Vec<(String, String)>>,
    phrases: RefCell<HashMap<String, String>>,
    /// Console lines for the server.
    pub to_server: RefCell<Vec<String>>,
    /// User info (ConVars with `FCVAR_USERINFO`) the server has not been told of.
    pub user_info: RefCell<Vec<(String, String)>>,
    /// Keys and buttons of the game's that went down (true) or up since the server was told,
    /// for its `PlayerButtonDown` and `PlayerButtonUp`; and those down, as it was told.
    pub(crate) buttons: RefCell<Vec<(i32, bool)>>,
    buttons_down: RefCell<HashSet<i32>>,
    pub frame: Cell<u64>,
    pub real_frame_time: Cell<f64>,
    /// The view is from outside the local player, who is drawn then (`CalcView`'s
    /// `drawviewer`).
    pub draw_viewer: Cell<bool>,
}

impl Default for Client {
    fn default() -> Client {
        Client {
            local_player: Cell::new(EntityHandle::NULL),
            awaiting_entities: Cell::new(false),
            view: Cell::new(View::default()),
            keys: RefCell::new(Keys::default()),
            cursor: Cell::new((0, 0)),
            screen_clicker: Cell::new(false),
            binds: RefCell::new(Vec::new()),
            phrases: RefCell::new(HashMap::new()),
            to_server: RefCell::new(Vec::new()),
            user_info: RefCell::new(Vec::new()),
            buttons: RefCell::new(Vec::new()),
            buttons_down: RefCell::new(HashSet::new()),
            frame: Cell::new(0),
            draw_viewer: Cell::new(false),
            real_frame_time: Cell::new(0.0),
        }
    }
}

impl Client {
    /// A key or mouse button goes down or up (`KEY_*`, `MOUSE_*`).
    pub fn set_key(&self, code: i32, down: bool) {
        let mut keys = self.keys.borrow_mut();
        if down {
            if keys.down.insert(code) {
                keys.pressed.insert(code);
                if keys.trapping {
                    keys.trapped = Some(code);
                    keys.trapping = false;
                }
            }
        } else if keys.down.remove(&code) {
            keys.released.insert(code);
        }
    }

    pub fn is_down(&self, code: i32) -> bool {
        self.keys.borrow().down.contains(&code)
    }

    /// The frame ends: what went down or up is old news.
    pub fn end_frame(&self) {
        let mut keys = self.keys.borrow_mut();
        keys.pressed.clear();
        keys.released.clear();
    }

    /// The command bound to a key, if any.
    pub fn binding(&self, code: i32) -> Option<String> {
        let name = key_name(code)?;
        self.binds.borrow().iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, command)| command.clone())
    }

    /// The phrase of a token (`#tool.weld.name` or `tool.weld.name`), or the token itself.
    pub fn phrase(&self, token: &str) -> String {
        let key = token.strip_prefix('#').unwrap_or(token);
        self.phrases.borrow().get(&key.to_lowercase()).cloned().unwrap_or_else(|| token.to_string())
    }
}

/// The engine's names of keys (`input.GetKeyName`, `bind`), by `KEY_*` / `MOUSE_*` code.
const KEY_NAMES: &[(i32, &str)] = &[
    (1, "0"),
    (2, "1"),
    (3, "2"),
    (4, "3"),
    (5, "4"),
    (6, "5"),
    (7, "6"),
    (8, "7"),
    (9, "8"),
    (10, "9"),
    (37, "KP_INS"),
    (38, "KP_END"),
    (39, "KP_DOWNARROW"),
    (40, "KP_PGDN"),
    (41, "KP_LEFTARROW"),
    (42, "KP_5"),
    (43, "KP_RIGHTARROW"),
    (44, "KP_HOME"),
    (45, "KP_UPARROW"),
    (46, "KP_PGUP"),
    (47, "KP_SLASH"),
    (48, "KP_MULTIPLY"),
    (49, "KP_MINUS"),
    (50, "KP_PLUS"),
    (51, "KP_ENTER"),
    (52, "KP_DEL"),
    (53, "["),
    (54, "]"),
    (55, "SEMICOLON"),
    (56, "'"),
    (57, "`"),
    (58, ","),
    (59, "."),
    (60, "/"),
    (61, "\\"),
    (62, "-"),
    (63, "="),
    (64, "ENTER"),
    (65, "SPACE"),
    (66, "BACKSPACE"),
    (67, "TAB"),
    (68, "CAPSLOCK"),
    (69, "NUMLOCK"),
    (70, "ESCAPE"),
    (71, "SCROLLLOCK"),
    (72, "INS"),
    (73, "DEL"),
    (74, "HOME"),
    (75, "END"),
    (76, "PGUP"),
    (77, "PGDN"),
    (78, "PAUSE"),
    (79, "SHIFT"),
    (80, "RSHIFT"),
    (81, "ALT"),
    (82, "RALT"),
    (83, "CTRL"),
    (84, "RCTRL"),
    (85, "LWIN"),
    (86, "RWIN"),
    (87, "APP"),
    (88, "UPARROW"),
    (89, "LEFTARROW"),
    (90, "DOWNARROW"),
    (91, "RIGHTARROW"),
    (92, "F1"),
    (93, "F2"),
    (94, "F3"),
    (95, "F4"),
    (96, "F5"),
    (97, "F6"),
    (98, "F7"),
    (99, "F8"),
    (100, "F9"),
    (101, "F10"),
    (102, "F11"),
    (103, "F12"),
    (107, "MOUSE1"),
    (108, "MOUSE2"),
    (109, "MOUSE3"),
    (110, "MOUSE4"),
    (111, "MOUSE5"),
    (112, "MWHEELUP"),
    (113, "MWHEELDOWN"),
];

/// The engine's name of a key: `a`..`z` for letters, else as [`KEY_NAMES`] has it.
pub fn key_name(code: i32) -> Option<&'static str> {
    const LETTERS: [&str; 26] =
        ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z"];
    if (11..=36).contains(&code) {
        return Some(LETTERS[(code - 11) as usize]);
    }
    KEY_NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// The code of a key by its engine name, any case.
pub fn key_code(name: &str) -> Option<i32> {
    if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
        return Some(11 + i32::from(name.to_ascii_lowercase().as_bytes()[0] - b'a'));
    }
    KEY_NAMES.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(c, _)| *c)
}

/// The registry's table of `DynamicLight`s, by number (an `elight`'s negative).
fn dynamic_lights_table(lua: &Lua) -> mlua::Result<Table> {
    const KEY: &str = "opengmod.dynamic_lights";
    if let Some(table) = lua.named_registry_value::<Option<Table>>(KEY)? {
        return Ok(table);
    }
    let table = lua.create_table()?;
    lua.set_named_registry_value(KEY, &table)?;
    Ok(table)
}

/// A light of the client's for a moment (`DynamicLight`): a point light that falls off to
/// nothing at its size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicLight {
    pub position: Vector,
    /// Linear light: the colour over 255 times 2 to the power of `Brightness`.
    pub color: [f32; 3],
    /// How far it reaches.
    pub radius: f32,
    /// It lights the world (not `noworld`), and models (not `nomodel`).
    pub world: bool,
    pub models: bool,
}

/// The `DynamicLight`s that still shine: those whose `DieTime` has not come, with a size.
/// Those gone are let go.
pub(crate) fn dynamic_lights(lua: &Lua) -> Vec<DynamicLight> {
    let Ok(lights) = dynamic_lights_table(lua) else { return Vec::new() };
    let now = host(lua).clock.cur_time.get();
    let mut shining = Vec::new();
    let mut gone = Vec::new();
    for pair in lights.pairs::<Value, Table>() {
        let Ok((key, light)) = pair else { continue };
        let number = |name: &str| light.raw_get::<Option<f64>>(name).ok().flatten();
        if number("DieTime").is_none_or(|t| t < now) {
            gone.push(key);
            continue;
        }
        let Some(Value::UserData(pos)) = light.raw_get::<Value>("Pos").ok() else { continue };
        let Ok(position) = pos.borrow::<Vector>().map(|p| *p) else { continue };
        let radius = number("Size").unwrap_or(0.0) as f32;
        if radius <= 0.0 {
            continue;
        }
        let scale = 2f32.powf(number("Brightness").unwrap_or(0.0) as f32) / 255.0;
        let channel = |name: &str| number(name).unwrap_or(0.0) as f32 * scale;
        let flag = |name: &str| matches!(light.raw_get::<Value>(name), Ok(Value::Boolean(true)));
        shining.push(DynamicLight {
            position,
            color: [channel("r"), channel("g"), channel("b")],
            radius,
            world: !flag("noworld"),
            models: !flag("nomodel"),
        });
    }
    for key in gone {
        let _ = lights.raw_set(key, Value::Nil);
    }
    shining
}

/// Reads the default key bindings (`bind "q" "+menu"`), and the phrases of the language.
pub(crate) fn load(lua: &Lua) {
    let host = host(lua);
    let vfs = host.vfs.clone();
    if let Ok(data) = vfs.read("cfg/config_default.cfg", "GAME") {
        let text = String::from_utf8_lossy(&data);
        let mut binds = host.client.binds.borrow_mut();
        for words in crate::convar::split_commands(&text) {
            if words.len() >= 3 && words[0].eq_ignore_ascii_case("bind") {
                binds.retain(|(key, _)| !key.eq_ignore_ascii_case(&words[1]));
                binds.push((words[1].to_lowercase(), words[2].clone()));
            }
        }
    }
    drop(host);
    load_phrases(lua);
}

/// The engine's name of a language GMod names by its code (`ru`: `russian`), as its token
/// files (`gameui_russian.txt`) are named.
fn engine_language(code: &str) -> &'static str {
    match code.to_ascii_lowercase().as_str() {
        "bg" => "bulgarian",
        "cs" => "czech",
        "da" => "danish",
        "de" => "german",
        "el" => "greek",
        "es-es" | "es" => "spanish",
        "fi" => "finnish",
        "fr" => "french",
        "hu" => "hungarian",
        "it" => "italian",
        "ja" => "japanese",
        "ko" => "koreana",
        "nl" => "dutch",
        "no" => "norwegian",
        "pl" => "polish",
        "pt-br" => "brazilian",
        "pt-pt" => "portuguese",
        "ro" => "romanian",
        "ru" => "russian",
        "sv-se" | "sv" => "swedish",
        "th" => "thai",
        "tr" => "turkish",
        "uk" => "ukrainian",
        "zh-cn" => "schinese",
        "zh-tw" => "tchinese",
        _ => "english",
    }
}

/// The phrases of the language (`gmod_language`): the engine's token files and GMod's, English
/// first and the language over it.
pub(crate) fn load_phrases(lua: &Lua) {
    let host = host(lua);
    let vfs = host.vfs.clone();
    let language = crate::convar::value(lua, "gmod_language").filter(|l| !l.is_empty()).unwrap_or_else(|| "en".to_string());
    let mut phrases = host.client.phrases.borrow_mut();
    phrases.clear();
    let mut names = vec!["english"];
    if engine_language(&language) != "english" {
        names.push(engine_language(&language));
    }
    for name in names {
        for folder in ["resource", "resource/language"] {
            for file in vfs.find(&format!("{folder}/*_{name}.txt"), "GAME").files {
                let Ok(data) = vfs.read(&format!("{folder}/{file}"), "GAME") else { continue };
                let (document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&data), gmod_kv::Options { escapes: true });
                let Some(tokens) = document.root.iter().find_map(|p| p.value.as_block()).and_then(|b| b.block("Tokens")) else { continue };
                for pair in tokens {
                    if let Some(value) = pair.value.as_str() {
                        phrases.insert(pair.key.to_lowercase(), value.to_string());
                    }
                }
            }
        }
    }
    let mut languages = vec!["en".to_string()];
    if language != "en" {
        languages.push(language);
    }
    for language in languages {
        let folder = format!("resource/localization/{language}");
        for file in vfs.find(&format!("{folder}/*.properties"), "GAME").files {
            let Ok(data) = vfs.read(&format!("{folder}/{file}"), "GAME") else { continue };
            for line in String::from_utf8_lossy(&data).lines() {
                let line = line.trim_start_matches('\u{feff}').trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    phrases.insert(key.trim().to_lowercase(), unescape(value.trim()));
                }
            }
        }
    }
}

/// A value of a `.properties` file: `\:` is `:`, `\n` a line break.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn def_many(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn nothing(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

fn yes(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Boolean(true))
}

fn no(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Boolean(false))
}

fn zero(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Integer(0))
}

fn userdata(lua: &Lua, value: impl mlua::UserData + 'static) -> mlua::Result<Value> {
    Ok(Value::UserData(lua.create_userdata(value)?))
}

fn key_arg(a: &Args) -> mlua::Result<i32> {
    Ok(a.number(1)? as i32)
}

/// `LoadPresets`: the presets of the tools and the post-processing, by type and name - the
/// game's (`settings/presets/<type>/*.txt`), then the player's, which win.
fn load_presets(a: &Args) -> mlua::Result<Value> {
    let host = host(a.lua);
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for kind in host.vfs.find("settings/presets/*", "GAME").dirs {
        for file in host.vfs.find(&format!("settings/presets/{kind}/*.txt"), "GAME").files {
            if let Ok(data) = host.vfs.read(&format!("settings/presets/{kind}/{file}"), "GAME") {
                files.push((kind.to_lowercase(), data));
            }
        }
    }
    if let Some(Ok(root)) = host.data_dir.borrow().as_ref().map(|d| d.dir("settings/presets")) {
        for kind in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let name = kind.file_name().to_string_lossy().to_lowercase();
            for file in std::fs::read_dir(kind.path()).into_iter().flatten().flatten() {
                if let Ok(data) = std::fs::read(file.path()) {
                    files.push((name.clone(), data));
                }
            }
        }
    }
    let out = a.lua.create_table()?;
    for (kind, data) in files {
        let (document, _) = gmod_kv::parse_lenient(&String::from_utf8_lossy(&data), gmod_kv::Options::default());
        let presets = match out.raw_get::<Option<Table>>(kind.as_str())? {
            Some(t) => t,
            None => {
                let t = a.lua.create_table()?;
                out.raw_set(kind.as_str(), &t)?;
                t
            }
        };
        for pair in document.root.iter() {
            if let Some(block) = pair.value.as_block() {
                presets.raw_set(pair.key.as_str(), crate::util::kv_block(a.lua, block, false, false)?)?;
            }
        }
    }
    Ok(Value::Table(out))
}

/// `SavePresets(presets)`: writes the presets of each type given to the player's folder.
fn save_presets(a: &Args) -> mlua::Result<Value> {
    let table = a.table(1)?;
    let host = host(a.lua);
    let dir = host.data_dir.borrow();
    let Some(dir) = dir.as_ref() else { return Ok(Value::Nil) };
    for pair in table.pairs::<String, Table>() {
        let (kind, presets) = pair?;
        let kind = safe_name(&kind);
        if let Ok(folder) = dir.dir(&format!("settings/presets/{kind}")) {
            for old in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let _ = std::fs::remove_file(old.path());
            }
        }
        for preset in presets.pairs::<String, Table>() {
            let (name, values) = preset?;
            let mut text = String::new();
            crate::util::write_keyvalues(a.lua, &mut text, &name, &values, 0)?;
            let _ = dir.write(&format!("settings/presets/{kind}/{}.txt", safe_name(&name)), text.as_bytes());
        }
    }
    Ok(Value::Nil)
}

/// `spawnmenu.PopulateFromTextFiles(callback)`: each spawn list - the player's
/// (`settings/spawnlist/`), else the game's, else its defaults (`settings/spawnlist_default/`) -
/// to `callback(file, name, contents, icon, id, parentid, needsapp)`.
fn populate_spawnlists(a: &Args) -> mlua::Result<Value> {
    let callback = a.function(1)?;
    let host = host(a.lua);
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    if let Some(Ok(root)) = host.data_dir.borrow().as_ref().map(|d| d.dir("settings/spawnlist")) {
        for file in std::fs::read_dir(root).into_iter().flatten().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if name.to_lowercase().ends_with(".txt")
                && let Ok(data) = std::fs::read(file.path())
            {
                files.push((name, data));
            }
        }
    }
    for folder in ["settings/spawnlist", "settings/spawnlist_default"] {
        if !files.is_empty() {
            break;
        }
        for file in host.vfs.find(&format!("{folder}/*.txt"), "GAME").files {
            if let Ok(data) = host.vfs.read(&format!("{folder}/{file}"), "GAME") {
                files.push((file, data));
            }
        }
    }
    files.sort_by(|x, y| x.0.cmp(&y.0));
    for (file, data) in files {
        let (document, _) = gmod_kv::parse_lenient(&String::from_utf8_lossy(&data), gmod_kv::Options::default());
        let Some(block) = document.root.iter().find_map(|p| p.value.as_block()) else { continue };
        let list = crate::util::kv_block(a.lua, block, false, false)?;
        let field = |k: &str| list.raw_get::<Value>(k);
        let contents = match field("contents")? {
            Value::Table(t) => Value::Table(t),
            _ => Value::Table(a.lua.create_table()?),
        };
        let name = match field("name")? {
            Value::Nil => Value::String(a.lua.create_string(file.trim_end_matches(".txt"))?),
            name => name,
        };
        let args = MultiValue::from_iter([
            Value::String(a.lua.create_string(&file)?),
            name,
            contents,
            field("icon")?,
            field("id")?,
            field("parentid")?,
            field("needsapp")?,
        ]);
        let _ = crate::files::call_reporting(a.lua, callback.clone(), args);
    }
    Ok(Value::Nil)
}

/// `spawnmenu.SaveToTextFiles(lists)`: the player's spawn lists, file name to keyvalues text,
/// in place of those there were.
fn save_spawnlists(a: &Args) -> mlua::Result<Value> {
    let lists = a.table(1)?;
    let host = host(a.lua);
    let dir = host.data_dir.borrow();
    let Some(dir) = dir.as_ref() else { return Ok(Value::Nil) };
    if let Ok(folder) = dir.dir("settings/spawnlist") {
        for old in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let _ = std::fs::remove_file(old.path());
        }
    }
    for pair in lists.pairs::<String, mlua::LuaString>() {
        let (name, text) = pair?;
        let _ = dir.write(&format!("settings/spawnlist/{}.txt", safe_name(&name)), &text.as_bytes());
    }
    Ok(Value::Nil)
}

/// A name made safe for a file's.
fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || " _-.".contains(c) { c } else { '_' })
        .collect::<String>()
        .trim_matches('.')
        .to_lowercase()
}

/// The direction from the eye through a pixel of the screen.
pub(crate) fn screen_to_vector(lua: &Lua, x: f32, y: f32) -> Vector {
    let host = host(lua);
    let view = host.client.view.get();
    let (w, h) = host.screen.size.get();
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    // the engine's field of view is for 4:3: wider screens see more at the sides
    let half = (view.fov.to_radians() / 2.0).tan() * (w / h) / (4.0 / 3.0);
    let dx = (2.0 * x / w - 1.0) * half;
    let dy = (1.0 - 2.0 * y / h) * half * h / w;
    let (forward, right, up) = view.angles.vectors();
    (forward + right * dx + up * dy).normalized()
}

/// Where a point of the world is on the screen, in pixels, as the view sees it, and whether
/// it is in front of the eyes - [`screen_to_vector`] the other way.
pub(crate) fn to_screen(lua: &Lua, point: Vector) -> (f32, f32, bool) {
    let host = host(lua);
    let view = host.client.view.get();
    let (w, h) = host.screen.size.get();
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let half = (view.fov.to_radians() / 2.0).tan() * (w / h) / (4.0 / 3.0);
    let (forward, right, up) = view.angles.vectors();
    let d = point - view.origin;
    let depth = d.dot(forward);
    let (sx, sy) = if depth.abs() < 1e-4 { (0.0, 0.0) } else { (d.dot(right) / depth / half, d.dot(up) / depth / (half * h / w)) };
    let x = (sx + 1.0) * w / 2.0;
    let y = (1.0 - sy) * h / 2.0;
    let visible = depth > 0.0 && (0.0..=w).contains(&x) && (0.0..=h).contains(&y);
    (x, y, visible)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    let vector_meta = crate::meta::meta_table(lua, "Vector", crate::meta::type_id::VECTOR)?;
    def(lua, &vector_meta, "ToScreen", |a| {
        let point = a.userdata::<Vector>(1, "Vector")?;
        let (x, y, visible) = to_screen(a.lua, point);
        let t = a.lua.create_table()?;
        t.raw_set("x", x)?;
        t.raw_set("y", y)?;
        t.raw_set("visible", visible)?;
        Ok(Value::Table(t))
    })?;
    def(lua, &globals, "LocalPlayer", |a| entity::push(a.lua, host(a.lua).client.local_player.get()))?;
    // models of the client's own: wheels, doors, gauges that addons draw with their entities
    def(lua, &globals, "ClientsideModel", |a| {
        let model = a.string(1)?.replace('\\', "/").to_ascii_lowercase();
        entity::push(a.lua, crate::ents::create_client_model(a.lua, &model).unwrap_or(EntityHandle::NULL))
    })?;
    def(lua, &globals, "GetViewEntity", |a| entity::push(a.lua, host(a.lua).client.local_player.get()))?;
    def(lua, &globals, "EyePos", |a| userdata(a.lua, host(a.lua).client.view.get().origin))?;
    def(lua, &globals, "EyeAngles", |a| userdata(a.lua, host(a.lua).client.view.get().angles))?;
    def(lua, &globals, "EyeVector", |a| userdata(a.lua, host(a.lua).client.view.get().angles.vectors().0))?;
    // the main view's: the same here, which draws no other views (`render.RenderView`) yet
    def(lua, &globals, "MainEyePos", |a| userdata(a.lua, host(a.lua).client.view.get().origin))?;
    def(lua, &globals, "MainEyeAngles", |a| userdata(a.lua, host(a.lua).client.view.get().angles))?;
    def(lua, &globals, "RenderAngles", |a| userdata(a.lua, host(a.lua).client.view.get().angles))?;
    def(lua, &globals, "RealFrameTime", |a| Ok(Value::Number(host(a.lua).client.real_frame_time.get())))?;
    def(lua, &globals, "FrameNumber", |a| Ok(Value::Integer(host(a.lua).client.frame.get() as i64)))?;
    def(lua, &globals, "VGUIFrameTime", |a| Ok(Value::Number(host(a.lua).started.elapsed().as_secs_f64())))?;
    def(lua, &globals, "DisableClipping", |a| {
        let f: mlua::Function = a.lua.globals().get::<Table>("surface")?.get("DisableClipping")?;
        f.call(a.boolean(1))
    })?;
    def(lua, &globals, "IsInGame", yes)?;
    def(lua, &globals, "ScreenScaleH", |a| {
        let h = host(a.lua).screen.size.get().1 as f64;
        Ok(Value::Number(a.number(1)? * h / 480.0))
    })?;
    def(lua, &globals, "AddWorldTip", nothing)?;
    def(lua, &globals, "CreateMaterial", |a| crate::material::push_named(a.lua, &a.string(1)?))?;
    def(lua, &globals, "GetRenderTarget", |a| crate::material::push_texture(a.lua, &a.string(1)?))?;
    def(lua, &globals, "GetRenderTargetEx", |a| crate::material::push_texture(a.lua, &a.string(1)?))?;
    // a light of the client's for a moment: one table a number, which Lua fills each frame
    def(lua, &globals, "DynamicLight", |a| {
        let index = a.number(1)? as i64;
        let elight = matches!(a.get(2), Some(Value::Boolean(true)));
        let lights = dynamic_lights_table(a.lua)?;
        let key = if elight { -index - 1 } else { index };
        if let Some(light) = lights.raw_get::<Option<Table>>(key)? {
            return Ok(Value::Table(light));
        }
        let light = a.lua.create_table()?;
        light.raw_set("key", index)?;
        lights.raw_set(key, &light)?;
        Ok(Value::Table(light))
    })?;
    def(lua, &globals, "LoadPresets", load_presets)?;
    def(lua, &globals, "SavePresets", save_presets)?;
    // the engine's half of `spawnmenu`: `includes/modules/spawnmenu.lua` keeps it and makes the
    // rest
    let spawnmenu = lua.create_table()?;
    def(lua, &spawnmenu, "PopulateFromTextFiles", populate_spawnlists)?;
    def(lua, &spawnmenu, "SaveToTextFiles", save_spawnlists)?;
    globals.raw_set("spawnmenu", spawnmenu)?;

    let input = lua.create_table()?;
    def(lua, &input, "IsKeyDown", |a| Ok(Value::Boolean(host(a.lua).client.is_down(key_arg(a)?))))?;
    def(lua, &input, "IsButtonDown", |a| Ok(Value::Boolean(host(a.lua).client.is_down(key_arg(a)?))))?;
    def(lua, &input, "IsMouseDown", |a| Ok(Value::Boolean(host(a.lua).client.is_down(key_arg(a)?))))?;
    def(lua, &input, "WasKeyPressed", |a| Ok(Value::Boolean(host(a.lua).client.keys.borrow().pressed.contains(&key_arg(a)?))))?;
    def(lua, &input, "WasMousePressed", |a| Ok(Value::Boolean(host(a.lua).client.keys.borrow().pressed.contains(&key_arg(a)?))))?;
    def(lua, &input, "WasKeyTyped", |a| Ok(Value::Boolean(host(a.lua).client.keys.borrow().pressed.contains(&key_arg(a)?))))?;
    def(lua, &input, "WasKeyReleased", |a| Ok(Value::Boolean(host(a.lua).client.keys.borrow().released.contains(&key_arg(a)?))))?;
    def(lua, &input, "WasMouseReleased", |a| Ok(Value::Boolean(host(a.lua).client.keys.borrow().released.contains(&key_arg(a)?))))?;
    def(lua, &input, "WasMouseDoublePressed", no)?;
    def(lua, &input, "IsShiftDown", |a| {
        let client = &host(a.lua).client;
        Ok(Value::Boolean(client.is_down(79) || client.is_down(80)))
    })?;
    def(lua, &input, "IsControlDown", |a| {
        let client = &host(a.lua).client;
        Ok(Value::Boolean(client.is_down(83) || client.is_down(84)))
    })?;
    def_many(lua, &input, "GetCursorPos", |a| {
        let (x, y) = host(a.lua).client.cursor.get();
        Ok(MultiValue::from_iter([Value::Integer(x.into()), Value::Integer(y.into())]))
    })?;
    def(lua, &input, "SetCursorPos", |a| {
        let (x, y) = (a.number(1)? as i32, a.number(2)? as i32);
        host(a.lua).client.cursor.set((x, y));
        Ok(Value::Nil)
    })?;
    def(lua, &input, "GetKeyName", |a| match key_name(key_arg(a)?) {
        Some(name) => Ok(Value::String(a.lua.create_string(name)?)),
        None => Ok(Value::Nil),
    })?;
    def(lua, &input, "GetKeyCode", |a| Ok(Value::Integer(key_code(&a.string(1)?).unwrap_or(0).into())))?;
    def(lua, &input, "LookupBinding", |a| {
        let binding = a.string(1)?;
        let exact = a.boolean(2);
        let host = host(a.lua);
        let binds = host.client.binds.borrow();
        let found = binds.iter().find(|(_, command)| {
            if exact { command.eq_ignore_ascii_case(&binding) } else { command.to_lowercase().contains(&binding.to_lowercase()) }
        });
        match found {
            Some((key, _)) => Ok(Value::String(a.lua.create_string(key)?)),
            None => Ok(Value::Nil),
        }
    })?;
    def(lua, &input, "LookupKeyBinding", |a| match host(a.lua).client.binding(key_arg(a)?) {
        Some(command) => Ok(Value::String(a.lua.create_string(command)?)),
        None => Ok(Value::Nil),
    })?;
    def(lua, &input, "TranslateAlias", nothing)?;
    def(lua, &input, "IsKeyTrapping", |a| Ok(Value::Boolean(host(a.lua).client.keys.borrow().trapping)))?;
    def(lua, &input, "StartKeyTrapping", |a| {
        let host = host(a.lua);
        let mut keys = host.client.keys.borrow_mut();
        keys.trapping = true;
        keys.trapped = None;
        Ok(Value::Nil)
    })?;
    def(lua, &input, "CheckKeyTrapping", |a| {
        Ok(host(a.lua).client.keys.borrow_mut().trapped.take().map_or(Value::Nil, |c| Value::Integer(c.into())))
    })?;
    def(lua, &input, "SelectWeapon", |a| {
        let weapon = entity::this(a, 1)?;
        let class = host(a.lua).entities.get(weapon).map(|ent| ent.class.clone());
        if let Some(class) = class {
            host(a.lua).client.to_server.borrow_mut().push(format!("use {class}"));
        }
        Ok(Value::Nil)
    })?;
    def(lua, &input, "GetAnalogValue", zero)?;
    def(lua, &input, "IsButtonDownAnalog", no)?;
    globals.raw_set("input", input)?;

    let gui = lua.create_table()?;
    def_many(lua, &gui, "MousePos", |a| {
        let (x, y) = host(a.lua).client.cursor.get();
        Ok(MultiValue::from_iter([Value::Integer(x.into()), Value::Integer(y.into())]))
    })?;
    def(lua, &gui, "MouseX", |a| Ok(Value::Integer(host(a.lua).client.cursor.get().0.into())))?;
    def(lua, &gui, "MouseY", |a| Ok(Value::Integer(host(a.lua).client.cursor.get().1.into())))?;
    def(lua, &gui, "SetMousePos", |a| {
        let (x, y) = (a.number(1)? as i32, a.number(2)? as i32);
        host(a.lua).client.cursor.set((x, y));
        Ok(Value::Nil)
    })?;
    def(lua, &gui, "EnableScreenClicker", |a| {
        host(a.lua).client.screen_clicker.set(a.boolean(1));
        Ok(Value::Nil)
    })?;
    def(lua, &gui, "IsGameUIVisible", no)?;
    def(lua, &gui, "IsConsoleVisible", no)?;
    def(lua, &gui, "ActivateGameUI", nothing)?;
    def(lua, &gui, "HideGameUI", nothing)?;
    def(lua, &gui, "OpenURL", |a| {
        let url = a.string(1)?;
        host(a.lua).print(&format!("Open in a browser: {url}\n"));
        Ok(Value::Nil)
    })?;
    def(lua, &gui, "ScreenToVector", |a| {
        let (x, y) = (a.number(1)? as f32, a.number(2)? as f32);
        userdata(a.lua, screen_to_vector(a.lua, x, y))
    })?;
    def(lua, &gui, "InternalCursorMoved", nothing)?;
    def(lua, &gui, "InternalMousePressed", nothing)?;
    def(lua, &gui, "InternalMouseReleased", nothing)?;
    def(lua, &gui, "InternalMouseDoublePressed", nothing)?;
    def(lua, &gui, "InternalMouseWheeled", nothing)?;
    def(lua, &gui, "InternalKeyCodePressed", nothing)?;
    def(lua, &gui, "InternalKeyCodeReleased", nothing)?;
    def(lua, &gui, "InternalKeyCodeTyped", nothing)?;
    def(lua, &gui, "InternalKeyTyped", nothing)?;
    def(lua, &gui, "IsInGame", yes)?;
    globals.raw_set("gui", gui)?;

    let language = lua.create_table()?;
    def(lua, &language, "Add", |a| {
        let (key, value) = (a.string(1)?, a.string(2)?);
        host(a.lua).client.phrases.borrow_mut().insert(key.trim_start_matches('#').to_lowercase(), value);
        Ok(Value::Nil)
    })?;
    def(lua, &language, "GetPhrase", |a| {
        let token = a.string(1)?;
        Ok(Value::String(a.lua.create_string(host(a.lua).client.phrase(&token))?))
    })?;
    globals.raw_set("language", language)?;

    install_render(lua, &globals)?;
    crate::render3d::install(lua)?;

    crate::chat::install(lua, &globals)?;

    let achievements = lua.create_table()?;
    for name in [
        "BalloonPopped",
        "EatBall",
        "Remover",
        "SpawnedNPC",
        "SpawnedProp",
        "SpawnedRagdoll",
        "SpawnMenuOpen",
        "IncBaddies",
        "IncBystander",
        "IncGoodies",
    ] {
        def(lua, &achievements, name, nothing)?;
    }
    def(lua, &achievements, "Count", zero)?;
    def(lua, &achievements, "GetCount", zero)?;
    def(lua, &achievements, "GetDesc", |a| Ok(Value::String(a.lua.create_string("")?)))?;
    def(lua, &achievements, "GetName", |a| Ok(Value::String(a.lua.create_string("")?)))?;
    def(lua, &achievements, "GetGoal", zero)?;
    def(lua, &achievements, "IsAchieved", no)?;
    globals.raw_set("achievements", achievements)?;
    Ok(())
}

/// `render` and `cam`: nothing is drawn in 3D from Lua yet, so these keep the state scripts
/// read back and do nothing else; `render.SetScissorRect` cuts the 2D drawing.
fn install_render(lua: &Lua, globals: &Table) -> mlua::Result<()> {
    let render = lua.create_table()?;
    for name in [
        "Clear",
        "ClearDepth",
        "ClearStencil",
        "ClearBuffersObeyStencil",
        "ClearStencilBufferRectangle",
        "SetColorModulation",
        "SetBlend",
        "SuppressEngineLighting",
        "SetMaterial",
        "SetLightingOrigin",
        "ResetModelLighting",
        "SetModelLighting",
        "SetLightingMode",
        "SetLocalModelLights",
        "SetAmbientLight",
        "SetShadowColor",
        "SetShadowDirection",
        "SetShadowDistance",
        "SetShadowsDisabled",
        "DrawScreenQuad",
        "DrawScreenQuadEx",
        "DrawTextureToScreen",
        "DrawTextureToScreenRect",
        "UpdateScreenEffectTexture",
        "UpdateFullScreenDepthTexture",
        "UpdatePowerOfTwoTexture",
        "UpdateRefractTexture",
        "PushRenderTarget",
        "PopRenderTarget",
        "SetRenderTarget",
        "SetRenderTargetEx",
        "CopyRenderTargetToTexture",
        "CopyTexture",
        "SetViewPort",
        "SetStencilEnable",
        "SetStencilCompareFunction",
        "SetStencilFailOperation",
        "SetStencilPassOperation",
        "SetStencilZFailOperation",
        "SetStencilReferenceValue",
        "SetStencilTestMask",
        "SetStencilWriteMask",
        "DrawSprite",
        "DrawBeam",
        "StartBeam",
        "AddBeam",
        "EndBeam",
        "DrawQuad",
        "DrawQuadEasy",
        "DrawLine",
        "DrawBox",
        "DrawWireframeBox",
        "DrawSphere",
        "DrawWireframeSphere",
        "Model",
        "MaterialOverride",
        "MaterialOverrideByIndex",
        "BrushMaterialOverride",
        "WorldMaterialOverride",
        "ModelMaterialOverride",
        "OverrideDepthEnable",
        "OverrideAlphaWriteEnable",
        "OverrideColorWriteEnable",
        "OverrideBlend",
        "OverrideBlendFunc",
        "SetWriteDepthToDestAlpha",
        "PushFilterMag",
        "PushFilterMin",
        "PopFilterMag",
        "PopFilterMin",
        "PushFlashlightMode",
        "PopFlashlightMode",
        "SetGoalToneMappingScale",
        "SetToneMappingScaleLinear",
        "TurnOnToneMapping",
        "FogMode",
        "FogStart",
        "FogEnd",
        "FogMaxDensity",
        "FogColor",
        "SetFogZ",
        "PushCustomClipPlane",
        "PopCustomClipPlane",
        "EnableClipping",
        "CullMode",
        "DepthRange",
        "RenderView",
        "RenderHUD",
        "RedownloadAllLightmaps",
        "Spin",
        "CapturePixels",
        "BlurRenderTarget",
        "DrawTextureToScreen",
        "BindLocalCubemap",
        "ClearRenderTarget",
        "PerformFullScreenStencilOperation",
        "RenderFlashlights",
        "ResetToneMappingScale",
        "SetColorMaterial",
        "SetColorMaterialIgnoreZ",
        "SetLightmapTexture",
    ] {
        def(lua, &render, name, nothing)?;
    }
    def(lua, &render, "GetMoBlurTex0", |a| crate::material::push_texture(a.lua, "_rt_MotionBlur"))?;
    def(lua, &render, "GetMoBlurTex1", |a| crate::material::push_texture(a.lua, "_rt_MotionBlur1"))?;
    def(lua, &render, "GetSmallTex0", |a| crate::material::push_texture(a.lua, "_rt_SmallHDR0"))?;
    def(lua, &render, "GetSmallTex1", |a| crate::material::push_texture(a.lua, "_rt_SmallHDR1"))?;
    def(lua, &render, "GetToneMappingScaleLinear", |a| userdata(a.lua, Vector::new(1.0, 1.0, 1.0)))?;
    def(lua, &render, "GetFogMaxDensity", |_| Ok(Value::Number(1.0)))?;
    def(lua, &render, "MaxTextureWidth", |_| Ok(Value::Integer(16384)))?;
    def(lua, &render, "MaxTextureHeight", |_| Ok(Value::Integer(16384)))?;
    def(lua, &render, "ComputePixelDiameterOfSphere", |a| {
        let radius = a.opt_number(2)?.unwrap_or(0.0);
        let view = host(a.lua).client.view.get();
        let distance = match a.get(1) {
            Some(Value::UserData(ud)) => ud.borrow::<Vector>().map(|p| (*p - view.origin).length()).unwrap_or(1.0),
            _ => 1.0,
        };
        let height = f64::from(host(a.lua).screen.size.get().1);
        let half = (f64::from(view.fov).to_radians() / 2.0).tan().max(1e-3);
        Ok(Value::Number(radius * 2.0 / (f64::from(distance).max(1e-3) * half) * height / 2.0))
    })?;
    def(lua, &render, "SetScissorRect", |a| {
        let rect = [a.number(1)? as i32, a.number(2)? as i32, a.number(3)? as i32, a.number(4)? as i32];
        let screen = &host(a.lua).screen;
        screen.scissor.set(a.boolean(5).then_some(gmod_ui::Clip { left: rect[0], top: rect[1], right: rect[2], bottom: rect[3] }));
        screen.update_clip();
        Ok(Value::Nil)
    })?;
    def(lua, &render, "GetDXLevel", |_| Ok(Value::Integer(95)))?;
    def(lua, &render, "SupportsHDR", yes)?;
    def(lua, &render, "SupportsPixelShaders_1_4", yes)?;
    def(lua, &render, "SupportsPixelShaders_2_0", yes)?;
    def(lua, &render, "SupportsVertexShaders_2_0", yes)?;
    def(lua, &render, "GetToneMappingScale", |a| userdata(a.lua, Vector::new(1.0, 1.0, 1.0)))?;
    def(lua, &render, "GetLightColor", |a| userdata(a.lua, Vector::new(1.0, 1.0, 1.0)))?;
    def(lua, &render, "ComputeLighting", |a| userdata(a.lua, Vector::new(1.0, 1.0, 1.0)))?;
    def(lua, &render, "ComputeDynamicLighting", |a| userdata(a.lua, Vector::default()))?;
    def(lua, &render, "GetAmbientLightColor", |a| userdata(a.lua, Vector::new(0.2, 0.2, 0.2)))?;
    def_many(lua, &render, "GetColorModulation", |a| (1.0, 1.0, 1.0).into_lua_multi(a.lua))?;
    def(lua, &render, "GetBlend", |_| Ok(Value::Number(1.0)))?;
    def(lua, &render, "GetFogMode", zero)?;
    def_many(lua, &render, "GetFogDistances", |a| (0, 0, 0).into_lua_multi(a.lua))?;
    def_many(lua, &render, "GetFogColor", |a| (0, 0, 0).into_lua_multi(a.lua))?;
    def(lua, &render, "GetRenderTarget", nothing)?;
    def(lua, &render, "GetScreenEffectTexture", |a| crate::material::push_texture(a.lua, "_rt_FullFrameFB"))?;
    def(lua, &render, "GetPowerOfTwoTexture", |a| crate::material::push_texture(a.lua, "_rt_PowerOfTwoFB"))?;
    def(lua, &render, "GetRefractTexture", |a| crate::material::push_texture(a.lua, "_rt_PowerOfTwoFB"))?;
    def(lua, &render, "GetMorphTex0", |a| crate::material::push_texture(a.lua, "_rt_morphtex0"))?;
    def(lua, &render, "GetMorphTex1", |a| crate::material::push_texture(a.lua, "_rt_morphtex1"))?;
    def(lua, &render, "GetSuperFPTex", |a| crate::material::push_texture(a.lua, "_rt_SuperFPTex"))?;
    def(lua, &render, "GetSuperFPTex2", |a| crate::material::push_texture(a.lua, "_rt_SuperFPTex2"))?;
    def(lua, &render, "GetFullScreenDepthTexture", |a| crate::material::push_texture(a.lua, "_rt_ResolvedFullFrameDepth"))?;
    def(lua, &render, "GetBloomTex0", |a| crate::material::push_texture(a.lua, "_rt_SmallFB0"))?;
    def(lua, &render, "GetBloomTex1", |a| crate::material::push_texture(a.lua, "_rt_SmallFB1"))?;
    def(lua, &render, "GetResolvedFullFrameDepth", |a| crate::material::push_texture(a.lua, "_rt_ResolvedFullFrameDepth"))?;
    def(lua, &render, "GetViewSetup", |a| {
        let t = a.lua.create_table()?;
        let view = host(a.lua).client.view.get();
        let (w, h) = host(a.lua).screen.size.get();
        t.raw_set("origin", view.origin)?;
        t.raw_set("angles", view.angles)?;
        t.raw_set("fov", view.fov)?;
        t.raw_set("x", 0)?;
        t.raw_set("y", 0)?;
        t.raw_set("width", w)?;
        t.raw_set("height", h)?;
        t.raw_set("aspect", w as f32 / h.max(1) as f32)?;
        t.raw_set("znear", 3)?;
        t.raw_set("zfar", 28377.9199)?;
        Ok(Value::Table(t))
    })?;
    def(lua, &render, "IsTakingScreenshot", no)?;
    def(lua, &render, "Capture", nothing)?;
    def(lua, &render, "ReadPixel", |_| Ok(Value::Integer(0)))?;
    def(lua, &render, "GetHDREnabled", yes)?;
    def(lua, &render, "GetSurfaceColor", |a| userdata(a.lua, Vector::default()))?;
    globals.raw_set("render", render)?;

    let cam = lua.create_table()?;
    for name in [
        "Start",
        "End",
        "Start2D",
        "End2D",
        "Start3D",
        "End3D",
        "StartOrthoView",
        "EndOrthoView",
        "PushModelMatrix",
        "PopModelMatrix",
        "IgnoreZ",
        "ApplyShake",
    ] {
        def(lua, &cam, name, nothing)?;
    }
    def(lua, &cam, "GetModelMatrix", |a| userdata(a.lua, crate::matrix::Matrix::IDENTITY))?;
    def(lua, &cam, "GetViewMatrix", |a| userdata(a.lua, crate::matrix::Matrix::IDENTITY))?;
    def(lua, &cam, "GetProjectionMatrix", |a| userdata(a.lua, crate::matrix::Matrix::IDENTITY))?;
    globals.raw_set("cam", cam)?;
    Ok(())
}

// --- the program's side --------------------------------------------------------------------

/// `MOUSE_WHEEL_UP`, `MOUSE_WHEEL_DOWN`.
const MOUSE_WHEEL_UP: i32 = 112;
const MOUSE_WHEEL_DOWN: i32 = 113;

/// The client's half of the program: what it is told of the screen and the player, the keys
/// and the cursor, and the frame of 2D it draws.
impl crate::Runtime {
    /// The size of the screen in pixels; Lua hears of a change (`OnScreenSizeChanged`).
    pub fn set_screen_size(&self, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        let old = host(&self.lua).screen.size.replace(size);
        if old != size {
            let args = (old.0, old.1, size.0, size.1).into_lua_multi(&self.lua).unwrap_or_default();
            self.call_hook("OnScreenSizeChanged", args);
        }
    }

    /// The player this client plays: `LocalPlayer()`.
    pub fn set_local_player(&self, player: EntityHandle) {
        host(&self.lua).client.local_player.set(player);
    }

    /// Where the local player sees from this frame.
    pub fn set_view(&self, view: View) {
        host(&self.lua).client.view.set(view);
    }

    /// The view as the client's Lua makes it from the player's eyes (`CalcView`, which the base
    /// gamemode hands to `CalcVehicleView` in a vehicle, to the player's class and weapon):
    /// where to see from, which way, how wide; and whether the player is drawn then.
    pub fn calc_view(&self, player: EntityHandle, view: View) -> View {
        calc_view(&self.lua, player, view)
    }

    /// The cursor moved, to a pixel of the screen.
    pub fn set_cursor(&self, x: i32, y: i32) {
        host(&self.lua).client.cursor.set((x, y));
        crate::vgui::cursor_moved(&self.lua);
    }

    /// Whether the cursor is out - a menu that takes the mouse shows, the chat box is open, or
    /// Lua asked for it (`gui.EnableScreenClicker`) - so the mouse does not turn the view.
    pub fn cursor_visible(&self) -> bool {
        let host = host(&self.lua);
        host.panels.cursor_visible(host.client.screen_clicker.get()) || crate::chat::is_open(&self.lua)
    }

    /// A key or mouse button (`KEY_*`, `MOUSE_*`) went down or up. The open chat box has the
    /// keys going down first ([`crate::chat`]), then the panels; then `PlayerBindPress` hears
    /// of the command bound to it, and a command of the client's own runs (`+menu` with the
    /// key down, `-menu` with it up, `messagemode`). True when the client took it; the game
    /// has the rest (`+attack`, `noclip` ...).
    pub fn button(&self, code: i32, down: bool) -> bool {
        let lua = &self.lua;
        // a key held down repeats: for the text entries, not the binds
        let repeat = down && host(lua).client.is_down(code);
        host(lua).client.set_key(code, down);
        // a key the game had going down goes up for it whoever takes it now
        if !down && host(lua).client.buttons_down.borrow_mut().remove(&code) {
            self.player_button(code, false);
        }
        if down {
            crate::chat::key_down(lua);
            if let Some(taken) = crate::chat::key(lua, code) {
                return taken;
            }
        } else if crate::chat::key_up(lua, code) {
            return true;
        }
        let taken = match code {
            107..=111 => crate::vgui::mouse_button(lua, code, down),
            MOUSE_WHEEL_UP | MOUSE_WHEEL_DOWN => down && crate::vgui::mouse_wheel(lua, if code == MOUSE_WHEEL_UP { 1 } else { -1 }),
            _ => crate::vgui::key(lua, code, down),
        };
        if taken {
            return true;
        }
        if repeat {
            return false;
        }
        if down && host(lua).client.buttons_down.borrow_mut().insert(code) {
            self.player_button(code, true);
        }
        let Some(command) = host(lua).client.binding(code) else { return false };
        if down {
            let player = entity::push(lua, host(lua).client.local_player.get()).unwrap_or(Value::Nil);
            let args = (player, command.as_str(), true, code).into_lua_multi(lua).unwrap_or_default();
            if matches!(crate::call_hook(lua, "PlayerBindPress", args).front(), Some(Value::Boolean(true))) {
                return true;
            }
        }
        let line = match command.strip_prefix('+') {
            Some(rest) if !down => format!("-{rest}"),
            _ if down => command,
            _ => return false,
        };
        let commands = crate::convar::split_commands(&line);
        if commands.is_empty() || !commands.iter().all(|words| crate::convar::is_known(lua, &words[0])) {
            return false;
        }
        for words in commands {
            crate::convar::execute(lua, &words);
        }
        true
    }

    /// Text was typed (what the keys made, after the keyboard's layout): it goes into the open
    /// chat box, or the text entry that has the keyboard. True when one took it.
    pub fn type_text(&self, text: &str) -> bool {
        crate::chat::type_text(&self.lua, text) || crate::vgui::text_typed(&self.lua, text)
    }

    /// Draws the frame's 2D: the panels think and are laid out, then the HUD - the engine's
    /// and Lua's - and the panels are painted, and `PostRenderVGUI` and `DrawOverlay` draw
    /// over them. [`with_canvas`](Self::with_canvas) has what was drawn.
    pub fn paint(&self, real_frame_time: f64) {
        let lua = &self.lua;
        {
            let host = host(lua);
            host.client.real_frame_time.set(real_frame_time);
            host.client.frame.set(host.client.frame.get() + 1);
            host.screen.begin();
        }
        crate::chat::advance(lua, real_frame_time);
        crate::vgui::think(lua);
        crate::vgui::layout(lua);
        crate::hud::paint(lua);
        crate::vgui::paint(lua);
        self.call_hook("PostRenderVGUI", MultiValue::new());
        self.call_hook("DrawOverlay", MultiValue::new());
        crate::vgui::remove_doomed(lua);
        host(lua).client.end_frame();
    }

    /// The frame [`paint`](Self::paint) drew, and the font atlas its text is cut from.
    pub fn with_canvas<R>(&self, f: impl FnOnce(&gmod_ui::Canvas, &mut gmod_ui::Fonts) -> R) -> R {
        let host = host(&self.lua);
        let canvas = host.screen.canvas.borrow();
        let mut fonts = host.screen.fonts.borrow_mut();
        f(&canvas, &mut fonts)
    }

    /// An image the frame draws ([`gmod_ui::Texture::Image`]): its name, and its pixels, read
    /// from a PNG file or a material's base texture. None when there is no such image.
    pub fn image(&self, id: u32) -> Option<(crate::surface::Image, Option<gmod_material::png::Rgba>)> {
        let image = host(&self.lua).screen.image(id)?;
        let pixels = crate::surface::decode(&self.lua, &image.name, false);
        Some((image, pixels))
    }

    /// Lets HTML panels show pages, which `pages` makes.
    pub fn set_web_pages(&self, pages: crate::html::WebPages) {
        *host(&self.lua).pages.web.borrow_mut() = Some(pages);
    }

    /// A key of choosing a weapon (the mouse wheel, a number key): the row of slots opens or
    /// its choice moves ([`crate::weaponselect`]). The command to send the server instead, when
    /// the HUD does not choose.
    pub fn choose_weapon(&self, choose: crate::weaponselect::Choose) -> Option<String> {
        crate::weaponselect::key(&self.lua, choose)
    }

    /// Whether the row of weapon slots shows: the attack keys are for it then.
    pub fn choosing_weapon(&self) -> bool {
        crate::weaponselect::is_open(&self.lua)
    }

    /// The attack key while the row shows: the command that takes the weapon chosen, if one is
    /// to be taken; the row goes away.
    pub fn take_chosen_weapon(&self) -> Option<String> {
        crate::weaponselect::take(&self.lua)
    }

    /// The second attack key while the row shows: it goes away.
    pub fn cancel_weapon_choice(&self) {
        crate::weaponselect::cancel(&self.lua);
    }

    /// The models to picture for spawn icons the game has none of, since the last call: the
    /// image each becomes ([`gmod_ui::Texture::Image`]), and the model; the program hands the
    /// pictures back with [`give_picture`](Self::give_picture).
    pub fn take_model_icons(&self) -> Vec<(u32, String)> {
        std::mem::take(&mut *host(&self.lua).screen.model_icons.borrow_mut())
    }

    /// A picture the program made for an image (a model's spawn icon): it comes back with
    /// [`take_page_pictures`](Self::take_page_pictures), to go to the GPU.
    pub fn give_picture(&self, id: u32, picture: gmod_material::png::Rgba) {
        host(&self.lua).pages.pictures.borrow_mut().insert(id, picture);
    }

    /// The pictures pages drew since the last call, by the image ([`gmod_ui::Texture::Image`])
    /// each is drawn as: to send the GPU before the frame is drawn.
    pub fn take_page_pictures(&self) -> Vec<(u32, gmod_material::png::Rgba)> {
        crate::html::take_pictures(&self.lua)
    }

    /// How many pages the HTML panels have, and how many of those drew a picture yet.
    pub fn page_counts(&self) -> (usize, usize) {
        crate::html::counts(&self.lua)
    }

    /// The console lines the client sends the server - the commands it does not know itself -
    /// oldest first.
    pub fn take_server_commands(&self) -> Vec<String> {
        std::mem::take(&mut *host(&self.lua).client.to_server.borrow_mut())
    }

    /// The keys and buttons of the game's (`KEY_*`, `MOUSE_*`) that went down (true) or up
    /// since the last time, in order: the server's `PlayerButtonDown` and `PlayerButtonUp`
    /// ([`player_button`](Self::player_button)).
    pub fn take_buttons(&self) -> Vec<(i32, bool)> {
        std::mem::take(&mut *host(&self.lua).client.buttons.borrow_mut())
    }

    /// The user info the client tells the server (`gmod_toolmode`, `cl_playermodel`): all of
    /// it the first time, then what changed.
    pub fn take_user_info(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *host(&self.lua).client.user_info.borrow_mut())
    }
}

/// `CalcView(ply, origin, angles, fov, znear, zfar)`: the view Lua gives back in its table -
/// `origin`, `angles`, `fov`, `drawviewer` - each where it says, else as it was.
fn calc_view(lua: &Lua, player: EntityHandle, view: View) -> View {
    // the view shakes before Lua sees it, as in Source
    let view = crate::shake::apply(lua, view);
    let client = &host(lua).client;
    client.draw_viewer.set(false);
    let args = (|| -> mlua::Result<MultiValue> {
        Ok(MultiValue::from_iter([
            entity::push(lua, player)?,
            entity::vector(lua, view.origin)?,
            entity::angle(lua, view.angles)?,
            Value::Number(f64::from(view.fov)),
            Value::Number(3.0),
            Value::Number(28377.9199),
        ]))
    })();
    let Ok(args) = args else { return view };
    let said = crate::call_hook(lua, "CalcView", args);
    let Some(Value::Table(t)) = said.front() else { return view };
    let mut out = view;
    if let Ok(Value::UserData(ud)) = t.raw_get::<Value>("origin")
        && let Ok(origin) = ud.borrow::<Vector>()
    {
        out.origin = *origin;
    }
    if let Ok(Value::UserData(ud)) = t.raw_get::<Value>("angles")
        && let Ok(angles) = ud.borrow::<Angle>()
    {
        out.angles = *angles;
    }
    if let Ok(Some(fov)) = t.raw_get::<Option<f64>>("fov")
        && fov.is_finite()
        && fov > 0.0
    {
        out.fov = fov as f32;
    }
    client.draw_viewer.set(matches!(t.raw_get::<Value>("drawviewer"), Ok(Value::Boolean(true))));
    out
}
