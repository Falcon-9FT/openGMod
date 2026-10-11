//! The chat: what players say to each other and what the engine tells them, and the chat box
//! a client draws (the engine's `CHudChat`).
//!
//! On the server `say <text>` and `say_team <text>` - typed into a console, or sent by the
//! chat box - go through `PlayerSay`, which may change the text or keep it quiet (""). Every
//! player `PlayerCanSeePlayersChat` lets see it gets it (without the hook: everyone, or the
//! speaker's team for `say_team`). What a player is told with `ChatPrint` or
//! `PrintMessage(HUD_PRINTTALK)` goes the same way, and so does `Player X has joined the game`
//! to the players already there. It all leaves through [`crate::Runtime::take_chat`].
//!
//! A client is handed a message with [`crate::Runtime::receive_chat`]: what a player said goes
//! to `OnPlayerChat(ply, text, team, dead)` - the base gamemode makes a line of it with
//! `chat.AddText`, the name in the colour of the player's team - and the engine's text to
//! `ChatText(index, name, text, type)`; true from either and the engine does not show it itself.
//! `chat.AddText` adds a line of coloured pieces to the box, and prints it to the console.
//!
//! The box opens with `messagemode` (Y) and `messagemode2` (U, for the team) or `chat.Open`:
//! `StartChat(team)`, where true hides the box - Lua draws a chat box of its own - while the
//! keys still go to it. Open, it has the keyboard and shows the cursor: what is typed edits the
//! line (`ChatTextChanged` hears every change, Tab asks `OnChatTab`), Enter sends it to the
//! server as `say`, Esc only closes; either calls `FinishChat`. Closed, a line shows for
//! `hud_saytext_time` seconds and then fades; open, the whole history shows over a dark panel,
//! and the mouse wheel and PageUp/PageDown scroll it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use gmod_ui::{FontId, FontSpec, Texture};
use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::entity::{self, EntityHandle};
use crate::{Realm, call_hook, host};

/// A message of the server's chat for one player.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatMessage {
    /// A player said something (`say`, `say_team`); `NULL` is the server's console.
    Said { speaker: EntityHandle, text: String, team: bool, dead: bool },
    /// The engine's text, of a type: `joinleave`, `none` for what a player is told.
    Text { player: u32, name: String, text: String, kind: String },
}

/// A line of the box: pieces of text in their colours.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    pieces: Vec<([u8; 4], String)>,
    /// When it came, by the chat's clock.
    born: f64,
}

/// How many lines the history keeps.
const HISTORY: usize = 100;
/// A line of chat is short: what the box lets be typed, in characters.
const MAX_CHARS: usize = 127;
/// Seconds a line takes to fade once its time is up.
const FADE: f64 = 1.0;
/// Text no colour was given for.
const WHITE: [u8; 4] = [255, 255, 255, 255];
/// A player's name when `team.GetColor` cannot tell (`TEAM_UNASSIGNED`'s yellow).
const UNASSIGNED: [u8; 4] = [255, 255, 100, 255];
const DEAD: [u8; 4] = [255, 30, 40, 255];
const TEAM: [u8; 4] = [30, 160, 40, 255];
/// The box behind the history while it is open, and behind the line being typed.
pub(crate) const PANEL: [u8; 4] = [20, 20, 20, 140];
pub(crate) const ENTRY: [u8; 4] = [0, 0, 0, 150];
/// Pixels between the box's edge and what is in it.
const PAD: f32 = 4.0;

const KEY_PAD_ENTER: i32 = 51;
const KEY_ENTER: i32 = 64;
const KEY_BACKSPACE: i32 = 66;
const KEY_TAB: i32 = 67;
const KEY_ESCAPE: i32 = 70;
const KEY_DELETE: i32 = 73;
const KEY_HOME: i32 = 74;
const KEY_END: i32 = 75;
const KEY_PAGEUP: i32 = 76;
const KEY_PAGEDOWN: i32 = 77;
const KEY_LEFT: i32 = 89;
const KEY_RIGHT: i32 = 91;
const MOUSE_WHEEL_UP: i32 = 112;
const MOUSE_WHEEL_DOWN: i32 = 113;

#[derive(Default)]
pub(crate) struct Chat {
    /// The server's messages, each with the player who gets it.
    sent: RefCell<Vec<(EntityHandle, ChatMessage)>>,
    lines: RefCell<VecDeque<Line>>,
    /// Open, and whether for the team only.
    open: Cell<Option<bool>>,
    /// `StartChat` said Lua draws the box.
    hidden: Cell<bool>,
    text: RefCell<String>,
    /// Where the caret is, in characters.
    caret: Cell<usize>,
    /// Rows the history is scrolled back by.
    scroll: Cell<usize>,
    /// The text of the key that opened or closed the box is not typed into it.
    swallow: Cell<bool>,
    /// Keys the box took going down, whose going up it takes too: no `-menu` for a Q typed.
    held: RefCell<Vec<i32>>,
    /// Seconds of the client's frames: what lines fade by.
    clock: Cell<f64>,
}

// --- the server's side ---------------------------------------------------------------------

/// The engine's commands of the chat: `say` and `say_team` on the server, `messagemode` and
/// `messagemode2` on a client. Whether `words` was one, which then ran.
pub(crate) fn command(lua: &Lua, player: EntityHandle, words: &[String]) -> bool {
    let Some((name, args)) = words.split_first() else { return false };
    let name = name.to_ascii_lowercase();
    match (host(lua).realm, name.as_str()) {
        (Realm::Server, "say" | "say_team") => say(lua, player, &args.join(" "), name == "say_team"),
        (Realm::Client, "messagemode" | "messagemode2") => open(lua, name == "messagemode2"),
        _ => return false,
    }
    true
}

/// Whether `name` is a command of the chat in this realm.
pub(crate) fn is_command(lua: &Lua, name: &str) -> bool {
    let names: &[&str] = match host(lua).realm {
        Realm::Server => &["say", "say_team"],
        Realm::Client => &["messagemode", "messagemode2"],
        Realm::Menu => &[],
    };
    names.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// A player (`NULL`: the server's console) says something: `PlayerSay` has it first, then it
/// goes to whoever may see it.
pub(crate) fn say(lua: &Lua, speaker: EntityHandle, text: &str, team: bool) {
    let mut text = text.to_string();
    let player = crate::player::state(lua, speaker, |_, _| ()).is_some();
    let ply = entity::push(lua, speaker).unwrap_or(Value::Nil);
    if player {
        let said = (ply.clone(), text.as_str(), team).into_lua_multi(lua).map(|args| call_hook(lua, "PlayerSay", args));
        if let Ok(said) = said
            && let Some(Value::String(s)) = said.front()
        {
            text = s.to_string_lossy();
        }
    }
    if text.is_empty() {
        return;
    }
    let dead = player && !crate::player::state(lua, speaker, |_, p| p.alive).unwrap_or(true);
    let speaker_team = crate::player::state(lua, speaker, |_, p| p.team);
    for listener in crate::player::all(lua) {
        let listener_team = crate::player::state(lua, listener, |_, p| p.team);
        let listener_value = entity::push(lua, listener).unwrap_or(Value::Nil);
        let args = (text.as_str(), team, listener_value, ply.clone()).into_lua_multi(lua).unwrap_or_default();
        let sees = match call_hook(lua, "PlayerCanSeePlayersChat", args).front() {
            Some(Value::Boolean(b)) => *b,
            _ => !team || (player && speaker_team == listener_team),
        };
        if sees {
            let message = ChatMessage::Said { speaker, text: text.clone(), team, dead };
            host(lua).chat.sent.borrow_mut().push((listener, message));
        }
    }
}

/// The engine tells a player something in the chat (`ChatPrint`, `HUD_PRINTTALK`).
pub(crate) fn tell(lua: &Lua, listener: EntityHandle, text: &str) {
    let message = ChatMessage::Text { player: 0, name: String::new(), text: text.to_string(), kind: "none".to_string() };
    host(lua).chat.sent.borrow_mut().push((listener, message));
}

/// A player joins or leaves: the others are told (`joinleave`).
pub(crate) fn join_leave(lua: &Lua, who: EntityHandle, name: &str, text: &str) {
    let player = who.index.unwrap_or(0);
    for listener in crate::player::all(lua).into_iter().filter(|&p| p != who) {
        let message = ChatMessage::Text { player, name: name.to_string(), text: text.to_string(), kind: "joinleave".to_string() };
        host(lua).chat.sent.borrow_mut().push((listener, message));
    }
}

// --- the client's side ---------------------------------------------------------------------

/// A message of the server arrives: `OnPlayerChat` or `ChatText`, and the engine's own line
/// unless they took it.
fn receive(lua: &Lua, message: &ChatMessage) {
    match message {
        ChatMessage::Said { speaker, text, team, dead } => {
            let ply = entity::push(lua, *speaker).unwrap_or(Value::Nil);
            let args = (ply, text.as_str(), *team, *dead).into_lua_multi(lua).unwrap_or_default();
            if is_true(&call_hook(lua, "OnPlayerChat", args)) {
                return;
            }
            let mut pieces = Vec::new();
            if *dead {
                pieces.push((DEAD, format!("{} ", phrase(lua, "chat.dead", "*DEAD*"))));
            }
            if *team {
                pieces.push((TEAM, format!("{} ", phrase(lua, "chat.team", "(TEAM)"))));
            }
            pieces.push(player_piece(lua, *speaker).unwrap_or((WHITE, phrase(lua, "chat.console", "Console"))));
            pieces.push((WHITE, format!(": {text}")));
            add_line(lua, pieces, true);
        }
        ChatMessage::Text { player, name, text, kind } => {
            let args = (*player, name.as_str(), text.as_str(), kind.as_str()).into_lua_multi(lua).unwrap_or_default();
            // the gamemode's `ChatText` prints it to the console itself
            if !is_true(&call_hook(lua, "ChatText", args)) {
                add_line(lua, vec![(WHITE, text.clone())], false);
            }
        }
    }
}

fn is_true(values: &MultiValue) -> bool {
    matches!(values.front(), Some(Value::Boolean(true)))
}

fn phrase(lua: &Lua, token: &str, english: &str) -> String {
    let phrase = host(lua).client.phrase(token);
    if phrase == token { english.to_string() } else { phrase }
}

/// A player's name in the colour of their team (`team.GetColor`). None for what is not a player.
fn player_piece(lua: &Lua, handle: EntityHandle) -> Option<([u8; 4], String)> {
    let (name, team) = crate::player::state(lua, handle, |_, p| (p.info.name.clone(), p.team))?;
    let color = || -> mlua::Result<Option<[u8; 4]>> {
        let Some(lib) = lua.globals().get::<Option<Table>>("team")? else { return Ok(None) };
        let Some(get) = lib.get::<Option<mlua::Function>>("GetColor")? else { return Ok(None) };
        match get.call::<Value>(team)? {
            Value::Table(t) => Ok(Some(crate::surface::color_table(&t)?)),
            _ => Ok(None),
        }
    };
    Some((color().ok().flatten().unwrap_or(UNASSIGNED), name))
}

/// Adds a line to the history, and to the console when `print`.
fn add_line(lua: &Lua, pieces: Vec<([u8; 4], String)>, print: bool) {
    let host = host(lua);
    if print {
        let mut text: String = pieces.iter().map(|(_, s)| s.as_str()).collect();
        text.push('\n');
        host.print(&text);
    }
    let chat = &host.chat;
    let mut lines = chat.lines.borrow_mut();
    lines.push_back(Line { pieces, born: chat.clock.get() });
    while lines.len() > HISTORY {
        lines.pop_front();
    }
}

/// `chat.AddText`: colours (`Color` tables) colour what follows, a player is their name in the
/// colour of their team, anything else is its text.
fn add_text(a: &Args) -> mlua::Result<Value> {
    let lua = a.lua;
    let mut color = WHITE;
    let mut pieces: Vec<([u8; 4], String)> = Vec::new();
    for value in a.values.iter() {
        match value {
            Value::Table(t) => {
                if matches!(t.raw_get::<Value>("r")?, Value::Integer(_) | Value::Number(_)) {
                    color = crate::surface::color_table(t)?;
                }
            }
            Value::String(s) => pieces.push((color, s.to_string_lossy())),
            Value::UserData(_) if entity::handle_of(Some(value)).is_some() => {
                let handle = entity::handle_of(Some(value)).unwrap_or(EntityHandle::NULL);
                pieces.push(player_piece(lua, handle).unwrap_or_else(|| (color, phrase(lua, "chat.console", "Console"))));
            }
            other => {
                let tostring: mlua::Function = lua.globals().get("tostring")?;
                pieces.push((color, tostring.call::<String>(other.clone())?));
            }
        }
    }
    add_line(lua, pieces, true);
    Ok(Value::Nil)
}

/// Opens the box: `StartChat`.
fn open(lua: &Lua, team: bool) {
    {
        let chat = &host(lua).chat;
        if chat.open.get().is_some() {
            return;
        }
        chat.open.set(Some(team));
        chat.text.borrow_mut().clear();
        chat.caret.set(0);
        chat.scroll.set(0);
        chat.swallow.set(true);
    }
    let hidden = is_true(&call_hook(lua, "StartChat", team.into_lua_multi(lua).unwrap_or_default()));
    host(lua).chat.hidden.set(hidden);
}

/// Closes the box: `FinishChat`.
fn close(lua: &Lua) {
    {
        let chat = &host(lua).chat;
        if chat.open.take().is_none() {
            return;
        }
        chat.text.borrow_mut().clear();
        chat.caret.set(0);
        chat.scroll.set(0);
        chat.hidden.set(false);
    }
    call_hook(lua, "FinishChat", MultiValue::new());
}

pub(crate) fn is_open(lua: &Lua) -> bool {
    host(lua).chat.open.get().is_some()
}

/// A new key goes down: what the key that opened or closed the box typed is past.
pub(crate) fn key_down(lua: &Lua) {
    host(lua).chat.swallow.set(false);
}

/// A key or button went down while the box is open. None when it is closed, or for the mouse's
/// buttons (the panels have those); else whether the box took it - it takes every key.
pub(crate) fn key(lua: &Lua, code: i32) -> Option<bool> {
    let team = host(lua).chat.open.get()?;
    if (107..=111).contains(&code) {
        return None;
    }
    host(lua).chat.held.borrow_mut().push(code);
    match code {
        KEY_ENTER | KEY_PAD_ENTER => {
            let text = host(lua).chat.text.borrow().clone();
            close(lua);
            host(lua).chat.swallow.set(true);
            if !text.trim().is_empty() {
                let command = if team { "say_team" } else { "say" };
                host(lua).client.to_server.borrow_mut().push(format!("{command} \"{}\"", text.replace('"', "'")));
            }
        }
        KEY_ESCAPE => {
            close(lua);
            host(lua).chat.swallow.set(true);
        }
        KEY_BACKSPACE | KEY_DELETE => {
            let chat = &host(lua).chat;
            let caret = chat.caret.get();
            let at = if code == KEY_BACKSPACE { caret.checked_sub(1) } else { Some(caret) };
            let removed = {
                let mut text = chat.text.borrow_mut();
                match at.and_then(|at| text.char_indices().nth(at)) {
                    Some((byte, _)) => {
                        text.remove(byte);
                        true
                    }
                    None => false,
                }
            };
            if removed {
                if code == KEY_BACKSPACE {
                    chat.caret.set(caret - 1);
                }
                changed(lua);
            }
        }
        KEY_LEFT | KEY_RIGHT | KEY_HOME | KEY_END => {
            let chat = &host(lua).chat;
            let (caret, len) = (chat.caret.get(), chat.text.borrow().chars().count());
            chat.caret.set(match code {
                KEY_LEFT => caret.saturating_sub(1),
                KEY_RIGHT => (caret + 1).min(len),
                KEY_HOME => 0,
                _ => len,
            });
        }
        KEY_TAB => {
            let text = host(lua).chat.text.borrow().clone();
            let args = text.as_str().into_lua_multi(lua).unwrap_or_default();
            if let Some(Value::String(s)) = call_hook(lua, "OnChatTab", args).front() {
                let new: String = s.to_string_lossy().chars().filter(|c| !c.is_control()).take(MAX_CHARS).collect();
                if new != text {
                    let chat = &host(lua).chat;
                    chat.caret.set(new.chars().count());
                    *chat.text.borrow_mut() = new;
                    changed(lua);
                }
            }
        }
        KEY_PAGEUP | MOUSE_WHEEL_UP => scroll(lua, if code == KEY_PAGEUP { 5 } else { 1 }),
        KEY_PAGEDOWN | MOUSE_WHEEL_DOWN => scroll(lua, if code == KEY_PAGEDOWN { -5 } else { -1 }),
        _ => {}
    }
    Some(true)
}

/// A key went up: whether the box took it going down.
pub(crate) fn key_up(lua: &Lua, code: i32) -> bool {
    let host = host(lua);
    let mut held = host.chat.held.borrow_mut();
    let before = held.len();
    held.retain(|&c| c != code);
    held.len() != before
}

fn scroll(lua: &Lua, rows: isize) {
    let chat = &host(lua).chat;
    chat.scroll.set(chat.scroll.get().saturating_add_signed(rows));
}

/// `ChatTextChanged`, with the line as it is now.
fn changed(lua: &Lua) {
    let text = host(lua).chat.text.borrow().clone();
    call_hook(lua, "ChatTextChanged", text.as_str().into_lua_multi(lua).unwrap_or_default());
}

/// Text was typed: into the line while the box is open. Whether the box took it.
pub(crate) fn type_text(lua: &Lua, typed: &str) -> bool {
    let chat = &host(lua).chat;
    if chat.swallow.replace(false) {
        return true;
    }
    if chat.open.get().is_none() {
        return false;
    }
    let inserted = {
        let mut text = chat.text.borrow_mut();
        let room = MAX_CHARS.saturating_sub(text.chars().count());
        let new: String = typed.chars().filter(|c| !c.is_control()).take(room).collect();
        let caret = chat.caret.get();
        let byte = text.char_indices().nth(caret).map_or(text.len(), |(i, _)| i);
        text.insert_str(byte, &new);
        chat.caret.set(caret + new.chars().count());
        !new.is_empty()
    };
    if inserted {
        changed(lua);
    }
    true
}

/// The chat's clock goes on by a frame.
pub(crate) fn advance(lua: &Lua, seconds: f64) {
    let chat = &host(lua).chat;
    chat.clock.set(chat.clock.get() + seconds.max(0.0));
}

// --- the box ---------------------------------------------------------------------------------

/// Where the box is, `chat.GetChatBoxPos` and `chat.GetChatBoxSize`: at the left, above the
/// health.
fn bounds(lua: &Lua) -> [f32; 4] {
    let h = host(lua).screen.size.get().1 as f32;
    [20.0, h - 340.0, 480.0, 240.0]
}

/// The font of the chat: `ChatFont` of the schemes, else one like it.
fn font(lua: &Lua) -> FontId {
    let screen = &host(lua).screen;
    if let Some(font) = screen.font("ChatFont") {
        return font;
    }
    let size = (screen.size.get().1 as f32 / 50.0).clamp(12.0, 22.0) as u32;
    let spec = FontSpec { family: "Verdana".into(), size, weight: 700, antialias: true, shadow: true, ..FontSpec::default() };
    screen.fonts.borrow_mut().font(&spec)
}

type Row = Vec<([u8; 4], String)>;

/// A line cut into rows no wider than `width`: at spaces, or anywhere in a word too long for a
/// row.
fn rows(fonts: &mut gmod_ui::Fonts, font: FontId, pieces: &[([u8; 4], String)], width: f32) -> Vec<Row> {
    fn push(row: &mut Row, color: [u8; 4], text: &str) {
        match row.last_mut() {
            Some((c, s)) if *c == color => s.push_str(text),
            _ => row.push((color, text.to_string())),
        }
    }
    let mut rows = vec![Vec::new()];
    let mut x = 0.0;
    for (color, text) in pieces {
        for (n, part) in text.split('\n').enumerate() {
            if n > 0 {
                rows.push(Vec::new());
                x = 0.0;
            }
            for word in part.split_inclusive(' ') {
                let wide = fonts.text_size(font, word).0 as f32;
                if x + wide > width && x > 0.0 {
                    rows.push(Vec::new());
                    x = 0.0;
                }
                if wide <= width {
                    push(rows.last_mut().expect("never empty"), *color, word);
                    x += wide;
                    continue;
                }
                for ch in word.chars() {
                    let mut buf = [0; 4];
                    let ch = ch.encode_utf8(&mut buf);
                    let wide = fonts.text_size(font, ch).0 as f32;
                    if x + wide > width && x > 0.0 {
                        rows.push(Vec::new());
                        x = 0.0;
                    }
                    push(rows.last_mut().expect("never empty"), *color, ch);
                    x += wide;
                }
            }
        }
    }
    rows
}

fn faded(color: [u8; 4], alpha: f64) -> [u8; 4] {
    [color[0], color[1], color[2], (f64::from(color[3]) * alpha).round().clamp(0.0, 255.0) as u8]
}

/// Draws the box: the lines still to be read while closed; open, the history over a panel and
/// the line being typed.
pub(crate) fn paint(lua: &Lua) {
    let [x, y, w, h] = bounds(lua);
    let font = font(lua);
    let (open, hidden, now) = {
        let chat = &host(lua).chat;
        (chat.open.get(), chat.hidden.get(), chat.clock.get())
    };
    let shown = open.filter(|_| !hidden);
    let time = crate::convar::value(lua, "hud_saytext_time").map_or(12.0, |v| crate::convar::atof(&v));
    let host = host(lua);
    let screen = &host.screen;
    let tall = screen.fonts.borrow().tall(font) as f32;
    let row_h = tall + 2.0;
    let entry_top = y + h - row_h - PAD;
    // the rows of the history, newest last, each with how much of it shows
    let mut history: Vec<(Row, f64)> = Vec::new();
    {
        let mut fonts = screen.fonts.borrow_mut();
        for line in host.chat.lines.borrow().iter() {
            let alpha = if shown.is_some() { 1.0 } else { ((line.born + time + FADE - now) / FADE).clamp(0.0, 1.0) };
            if alpha <= 0.0 {
                continue;
            }
            for row in rows(&mut fonts, font, &line.pieces, w - 2.0 * PAD) {
                history.push((row, alpha));
            }
        }
    }
    let fit = ((entry_top - y - PAD) / row_h).floor().max(0.0) as usize;
    let scroll = if shown.is_some() {
        let most = history.len().saturating_sub(fit);
        let scroll = host.chat.scroll.get().min(most);
        host.chat.scroll.set(scroll);
        scroll
    } else {
        0
    };
    if let Some(team) = shown {
        let mut canvas = screen.canvas.borrow_mut();
        canvas.image(Texture::White, false, [x, y, w, h], [0.0, 0.0, 1.0, 1.0], screen.color(PANEL));
        canvas.image(Texture::White, false, [x + PAD, entry_top, w - 2.0 * PAD, row_h], [0.0, 0.0, 1.0, 1.0], screen.color(ENTRY));
        drop(canvas);
        let prompt = if team { "Say (TEAM) :" } else { "Say :" };
        let text = host.chat.text.borrow().clone();
        let tx = x + 2.0 * PAD + screen.text(font, x + 2.0 * PAD, entry_top + 1.0, prompt, WHITE) + 4.0;
        screen.text(font, tx, entry_top + 1.0, &text, WHITE);
        let before: String = text.chars().take(host.chat.caret.get()).collect();
        let cx = tx + screen.fonts.borrow_mut().text_size(font, &before).0 as f32;
        let mut canvas = screen.canvas.borrow_mut();
        canvas.image(Texture::White, false, [cx, entry_top + 1.0, 1.0, tall], [0.0, 0.0, 1.0, 1.0], screen.color(WHITE));
    }
    let end = history.len() - scroll;
    let start = end.saturating_sub(fit);
    for (n, (row, alpha)) in history[start..end].iter().enumerate() {
        let ry = entry_top - PAD - (end - start - n) as f32 * row_h;
        let mut rx = x + PAD;
        for (color, text) in row {
            rx += screen.text(font, rx, ry, text, faded(*color, *alpha));
        }
    }
}

pub(crate) fn install(lua: &Lua, globals: &Table) -> mlua::Result<()> {
    let chat = lua.create_table()?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        chat.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("AddText", add_text)?;
    def("PlaySound", |a| {
        crate::sound::play_ui(a.lua, "common/talk.wav");
        Ok(Value::Nil)
    })?;
    // 1 for everyone, anything else for the team
    def("Open", |a| {
        let mode = a.opt_number(1)?.unwrap_or(1.0);
        open(a.lua, mode != 1.0);
        Ok(Value::Nil)
    })?;
    def("Close", |a| {
        close(a.lua);
        Ok(Value::Nil)
    })?;
    chat.raw_set(
        "GetChatBoxPos",
        lua.create_function(|lua, ()| {
            let [x, y, _, _] = bounds(lua);
            Ok((x as i64, y as i64))
        })?,
    )?;
    chat.raw_set(
        "GetChatBoxSize",
        lua.create_function(|lua, ()| {
            let [_, _, w, h] = bounds(lua);
            Ok((w as i64, h as i64))
        })?,
    )?;
    globals.raw_set("chat", chat)?;
    Ok(())
}

/// The lines of the history as text, oldest first.
#[cfg(test)]
pub(crate) fn lines(lua: &Lua) -> Vec<String> {
    host(lua).chat.lines.borrow().iter().map(|l| l.pieces.iter().map(|(_, s)| s.as_str()).collect()).collect()
}

/// The pieces of the newest line.
#[cfg(test)]
pub(crate) fn last_pieces(lua: &Lua) -> Vec<([u8; 4], String)> {
    host(lua).chat.lines.borrow().back().map(|l| l.pieces.clone()).unwrap_or_default()
}

/// The chat of the program's side.
impl crate::Runtime {
    /// The server's chat since the last call, oldest first: each message with the player who
    /// gets it.
    pub fn take_chat(&self) -> Vec<(EntityHandle, ChatMessage)> {
        std::mem::take(&mut *host(&self.lua).chat.sent.borrow_mut())
    }

    /// A message of the server's chat arrives at this client.
    pub fn receive_chat(&self, message: &ChatMessage) {
        receive(&self.lua, message);
    }

    /// Whether the chat box is open: it has the keyboard, and Esc closes it.
    pub fn chat_open(&self) -> bool {
        is_open(&self.lua)
    }
}
