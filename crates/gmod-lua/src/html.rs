//! The engine's side of HTML panels (`HTML`, `DHTML`, the main menu): each has a web page the
//! program makes ([`WebPages`]) - Chromium in openGMOD - drawn where the panel is.
//!
//! What a page tells comes to the panel's Lua as GMod's engine has it: `OnBeginLoadingDocument`,
//! `OnDocumentReady` and `OnFinishLoadingDocument`, `OnChangeTitle`, `ConsoleMessage`, and
//! `OnCallback` when the page calls a function Lua gave it (`NewObjectCallback`); a function the
//! page passes last is called back with what `OnCallback` returned. The mouse and the keys of
//! the panel go to its page.
//!
//! Without pages (no program gave any, or a page could not be made) the panels answer as
//! empty ones that never load.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gmod_material::png::Rgba;
use mlua::{Lua, MultiValue, Value};

use crate::host;
use crate::vgui::{self, PanelHandle};

/// A mouse button, as pages have them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebButton {
    Left,
    Middle,
    Right,
}

/// Keys and buttons held while the mouse or a key does something.
pub mod modifiers {
    pub const SHIFT: u32 = 1 << 1;
    pub const CONTROL: u32 = 1 << 2;
    pub const ALT: u32 = 1 << 3;
    pub const LEFT_BUTTON: u32 = 1 << 4;
    pub const MIDDLE_BUTTON: u32 = 1 << 5;
    pub const RIGHT_BUTTON: u32 = 1 << 6;
}

/// What a page tells.
#[derive(Debug, Clone, PartialEq)]
pub enum WebEvent {
    /// It began loading a document.
    Loading {
        url: String,
    },
    /// It loaded one.
    Loaded {
        url: String,
    },
    Title(String),
    /// `console.log` and the like, and errors: `level` is `debug`, `info`, `warn` or `error`.
    Console {
        message: String,
        source: String,
        line: i32,
        level: &'static str,
    },
    /// A function Lua gave the page was called, its arguments a JSON array; with a `callback`
    /// the page waits for what Lua returns ([`WebPage::answer`]).
    Callback {
        object: String,
        function: String,
        callback: Option<u64>,
        args: String,
    },
}

/// A web page, drawn off screen.
pub trait WebPage {
    fn open_url(&self, url: &str);
    /// Shows a document given whole.
    fn set_html(&self, html: &str);
    fn run_js(&self, js: &str);
    /// Gives the page `object.function`: calls of it come back as [`WebEvent::Callback`].
    fn add_function(&self, object: &str, function: &str);
    /// What Lua returned for a call that waits: a JSON array.
    fn answer(&self, callback: u64, values: &str);
    fn is_loading(&self) -> bool;
    fn stop(&self);
    fn reload(&self);
    fn go_back(&self);
    fn go_forward(&self);
    fn resize(&self, width: u32, height: u32);
    /// The newest picture of the page, if it drew one since the last call: RGBA, its colours
    /// not multiplied by alpha.
    fn take_picture(&self) -> Option<Rgba>;
    /// What the page told since the last call, oldest first.
    fn take_events(&self) -> Vec<WebEvent>;
    fn focus(&self, on: bool);
    fn mouse_move(&self, x: i32, y: i32, modifiers: u32);
    fn mouse_leave(&self);
    fn mouse_button(&self, x: i32, y: i32, button: WebButton, down: bool, modifiers: u32);
    /// The wheel turned by `notches` (up positive).
    fn mouse_wheel(&self, x: i32, y: i32, notches: i32, modifiers: u32);
    /// A key, by its Windows virtual-key code.
    fn key(&self, virtual_key: i32, down: bool, modifiers: u32);
    fn type_text(&self, text: &str, modifiers: u32);
}

/// Makes a page `width` by `height` pixels, empty; none when it cannot.
pub type WebPages = Rc<dyn Fn(u32, u32) -> Option<Box<dyn WebPage>>>;

struct Slot {
    page: Box<dyn WebPage>,
    /// The image the panel draws the page as.
    image: u32,
    /// It drew at least once.
    drawn: bool,
    size: (u32, u32),
}

/// The pages of a realm's panels.
#[derive(Default)]
pub(crate) struct Pages {
    pub web: RefCell<Option<WebPages>>,
    slots: RefCell<HashMap<PanelHandle, Slot>>,
    /// The newest pictures, by image number, until the program takes them.
    pub(crate) pictures: RefCell<HashMap<u32, Rgba>>,
}

/// Does something with a panel's page, making it first if the panel has none yet.
pub(crate) fn with_page<R>(lua: &Lua, h: PanelHandle, f: impl FnOnce(&dyn WebPage) -> R) -> Option<R> {
    let host = host(lua);
    let pages = &host.pages;
    if !pages.slots.borrow().contains_key(&h) {
        let web = pages.web.borrow().clone()?;
        let (w, hh) = vgui::size_of(lua, h)?;
        let size = (w.max(1) as u32, hh.max(1) as u32);
        let page = web(size.0, size.1)?;
        let image = host.screen.image_id(&format!("\u{1}page/{}/{}", h.index, h.serial), false);
        pages.slots.borrow_mut().insert(h, Slot { page, image, drawn: false, size });
    }
    let slots = pages.slots.borrow();
    Some(f(slots.get(&h)?.page.as_ref()))
}

/// Does something with a panel's page if it has one.
fn if_page<R>(lua: &Lua, h: PanelHandle, f: impl FnOnce(&dyn WebPage) -> R) -> Option<R> {
    let host = host(lua);
    let slots = host.pages.slots.borrow();
    Some(f(slots.get(&h)?.page.as_ref()))
}

/// The panel is gone: so is its page.
pub(crate) fn forget(lua: &Lua, h: PanelHandle) {
    let slot = host(lua).pages.slots.borrow_mut().remove(&h);
    drop(slot);
}

/// The image a panel draws its page as, once the page drew.
pub(crate) fn image_of(lua: &Lua, h: PanelHandle) -> Option<u32> {
    host(lua).pages.slots.borrow().get(&h).filter(|s| s.drawn).map(|s| s.image)
}

/// How many pages there are, and how many of them drew.
pub(crate) fn counts(lua: &Lua) -> (usize, usize) {
    let host = host(lua);
    let slots = host.pages.slots.borrow();
    (slots.len(), slots.values().filter(|s| s.drawn).count())
}

/// The pictures pages drew since the last call, by image number.
pub(crate) fn take_pictures(lua: &Lua) -> Vec<(u32, Rgba)> {
    host(lua).pages.pictures.borrow_mut().drain().collect()
}

/// Each frame: the pages are the size of their panels, their pictures are kept for the
/// program, and what they told reaches Lua.
pub(crate) fn update(lua: &Lua) {
    let mut told = Vec::new();
    {
        let host = host(lua);
        let mut slots = host.pages.slots.borrow_mut();
        slots.retain(|h, _| host.panels.is_valid(*h));
        for (&h, slot) in slots.iter_mut() {
            if let Some((w, hh)) = vgui::size_of(lua, h) {
                let size = (w.max(1) as u32, hh.max(1) as u32);
                if size != slot.size {
                    slot.size = size;
                    slot.page.resize(size.0, size.1);
                }
            }
            if let Some(picture) = slot.page.take_picture() {
                slot.drawn = true;
                host.pages.pictures.borrow_mut().insert(slot.image, picture);
            }
            let events = slot.page.take_events();
            if !events.is_empty() {
                told.push((h, events));
            }
        }
    }
    for (h, events) in told {
        for event in events {
            if !host(lua).panels.is_valid(h) {
                break;
            }
            tell(lua, h, event);
        }
    }
}

/// What a page told, to its panel's Lua.
fn tell(lua: &Lua, h: PanelHandle, event: WebEvent) {
    match event {
        WebEvent::Loading { url } => {
            vgui::call(lua, h, "OnBeginLoadingDocument", url);
        }
        WebEvent::Loaded { url } => {
            vgui::call(lua, h, "OnDocumentReady", url.clone());
            vgui::call(lua, h, "OnFinishLoadingDocument", url);
        }
        WebEvent::Title(title) => {
            vgui::call(lua, h, "OnChangeTitle", title);
        }
        WebEvent::Console { message, source, line, level } => {
            vgui::call(lua, h, "ConsoleMessage", (message, source, line, level));
        }
        WebEvent::Callback { object, function, callback, args } => {
            let args = match crate::util::from_json(lua, &args) {
                Some(Value::Table(t)) => Value::Table(t),
                _ => lua.create_table().map(Value::Table).unwrap_or(Value::Nil),
            };
            let returned = vgui::call(lua, h, "OnCallback", (object, function, args)).unwrap_or_default();
            if let Some(id) = callback {
                let values = answer_json(lua, returned);
                if_page(lua, h, |page| page.answer(id, &values));
            }
        }
    }
}

/// What Lua returned, as a JSON array.
fn answer_json(lua: &Lua, values: MultiValue) -> String {
    let parts: Vec<String> = values.iter().map(|v| crate::util::to_json(lua, v).unwrap_or_else(|_| "null".to_string())).collect();
    format!("[{}]", parts.join(","))
}

// --- the mouse and the keys ---------------------------------------------------------------

const KEY_LSHIFT: i32 = 79;
const KEY_RSHIFT: i32 = 80;
const KEY_LALT: i32 = 81;
const KEY_RALT: i32 = 82;
const KEY_LCONTROL: i32 = 83;
const KEY_RCONTROL: i32 = 84;
const MOUSE_LEFT: i32 = 107;
const MOUSE_RIGHT: i32 = 108;
const MOUSE_MIDDLE: i32 = 109;

/// The keys and buttons held now.
fn held(lua: &Lua) -> u32 {
    let client = &host(lua).client;
    let mut bits = 0;
    for (codes, bit) in [
        (&[KEY_LSHIFT, KEY_RSHIFT][..], modifiers::SHIFT),
        (&[KEY_LCONTROL, KEY_RCONTROL][..], modifiers::CONTROL),
        (&[KEY_LALT, KEY_RALT][..], modifiers::ALT),
        (&[MOUSE_LEFT][..], modifiers::LEFT_BUTTON),
        (&[MOUSE_MIDDLE][..], modifiers::MIDDLE_BUTTON),
        (&[MOUSE_RIGHT][..], modifiers::RIGHT_BUTTON),
    ] {
        if codes.iter().any(|&c| client.is_down(c)) {
            bits |= bit;
        }
    }
    bits
}

/// The cursor, in a panel's pixels.
fn local_cursor(lua: &Lua, h: PanelHandle) -> (i32, i32) {
    let (x, y) = host(lua).client.cursor.get();
    let (sx, sy) = host(lua).panels.screen_pos(h);
    (x - sx, y - sy)
}

pub(crate) fn mouse_moved(lua: &Lua, h: PanelHandle) {
    let (x, y) = local_cursor(lua, h);
    let held = held(lua);
    if_page(lua, h, |page| page.mouse_move(x, y, held));
}

pub(crate) fn mouse_left(lua: &Lua, h: PanelHandle) {
    if_page(lua, h, |page| page.mouse_leave());
}

pub(crate) fn mouse_button(lua: &Lua, h: PanelHandle, code: i32, down: bool) {
    let button = match code {
        MOUSE_LEFT => WebButton::Left,
        MOUSE_RIGHT => WebButton::Right,
        MOUSE_MIDDLE => WebButton::Middle,
        _ => return,
    };
    let (x, y) = local_cursor(lua, h);
    let held = held(lua);
    if_page(lua, h, |page| {
        if down {
            page.focus(true);
        }
        page.mouse_button(x, y, button, down, held);
    });
}

pub(crate) fn mouse_wheel(lua: &Lua, h: PanelHandle, notches: i32) -> bool {
    let (x, y) = local_cursor(lua, h);
    let held = held(lua);
    if_page(lua, h, |page| page.mouse_wheel(x, y, notches, held)).is_some()
}

pub(crate) fn key(lua: &Lua, h: PanelHandle, code: i32, down: bool) {
    let Some(vk) = virtual_key(code) else { return };
    let held = held(lua);
    if_page(lua, h, |page| page.key(vk, down, held));
}

pub(crate) fn type_text(lua: &Lua, h: PanelHandle, text: &str) {
    let held = held(lua);
    if_page(lua, h, |page| page.type_text(text, held));
}

pub(crate) fn focus(lua: &Lua, h: PanelHandle, on: bool) {
    if_page(lua, h, |page| page.focus(on));
}

/// The Windows virtual-key code of a key (`KEY_*`).
pub(crate) fn virtual_key(code: i32) -> Option<i32> {
    Some(match code {
        1..=10 => 0x30 + code - 1,   // KEY_0 ... KEY_9
        11..=36 => 0x41 + code - 11, // KEY_A ... KEY_Z
        37..=46 => 0x60 + code - 37, // KEY_PAD_0 ... KEY_PAD_9
        47 => 0x6F,                  // KEY_PAD_DIVIDE
        48 => 0x6A,                  // KEY_PAD_MULTIPLY
        49 => 0x6D,                  // KEY_PAD_MINUS
        50 => 0x6B,                  // KEY_PAD_PLUS
        51 | 64 => 0x0D,             // KEY_PAD_ENTER, KEY_ENTER
        52 => 0x6E,                  // KEY_PAD_DECIMAL
        53 => 0xDB,                  // KEY_LBRACKET
        54 => 0xDD,                  // KEY_RBRACKET
        55 => 0xBA,                  // KEY_SEMICOLON
        56 => 0xDE,                  // KEY_APOSTROPHE
        57 => 0xC0,                  // KEY_BACKQUOTE
        58 => 0xBC,                  // KEY_COMMA
        59 => 0xBE,                  // KEY_PERIOD
        60 => 0xBF,                  // KEY_SLASH
        61 => 0xDC,                  // KEY_BACKSLASH
        62 => 0xBD,                  // KEY_MINUS
        63 => 0xBB,                  // KEY_EQUAL
        65 => 0x20,                  // KEY_SPACE
        66 => 0x08,                  // KEY_BACKSPACE
        67 => 0x09,                  // KEY_TAB
        68 => 0x14,                  // KEY_CAPSLOCK
        69 => 0x90,                  // KEY_NUMLOCK
        70 => 0x1B,                  // KEY_ESCAPE
        71 => 0x91,                  // KEY_SCROLLLOCK
        72 => 0x2D,                  // KEY_INSERT
        73 => 0x2E,                  // KEY_DELETE
        74 => 0x24,                  // KEY_HOME
        75 => 0x23,                  // KEY_END
        76 => 0x21,                  // KEY_PAGEUP
        77 => 0x22,                  // KEY_PAGEDOWN
        78 => 0x13,                  // KEY_BREAK
        KEY_LSHIFT | KEY_RSHIFT => 0x10,
        KEY_LALT | KEY_RALT => 0x12,
        KEY_LCONTROL | KEY_RCONTROL => 0x11,
        85 => 0x5B,                   // KEY_LWIN
        86 => 0x5C,                   // KEY_RWIN
        87 => 0x5D,                   // KEY_APP
        88 => 0x26,                   // KEY_UP
        89 => 0x25,                   // KEY_LEFT
        90 => 0x28,                   // KEY_DOWN
        91 => 0x27,                   // KEY_RIGHT
        92..=103 => 0x70 + code - 92, // KEY_F1 ... KEY_F12
        _ => return None,
    })
}
