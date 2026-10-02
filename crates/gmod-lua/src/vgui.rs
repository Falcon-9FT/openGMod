//! VGUI: the panels of a client's screen, which Derma, the spawn menu, the scoreboard and
//! every addon's window are made of.
//!
//! The engine keeps the tree of panels: where each one is, its size, whether it shows,
//! docking, the order they are drawn in, which one the cursor is over and which one has the
//! keyboard. Lua gives them their behaviour: a panel's table (`GetTable`), filled by
//! `vgui.Create` from the control `vgui.Register` made, holds `Paint`, `PerformLayout`,
//! `OnMousePressed`..., which the engine calls. The engine's own classes are few: `Panel` and
//! `EditablePanel`, `Label` (text), `Button`, `TextEntry` (text typed in), `RichText` (lines
//! of coloured text, the chat's), `HTML` (pages, not shown here) and `ModelImage` (a spawn
//! icon).
//!
//! Each frame the panels think (`Think`, `AnimationThink`), are laid out (docking, then
//! `PerformLayout`) and are painted: the background, `Paint`, the engine's own drawing (a
//! label's text), the children in their order, `PaintOver`. Popups (`MakePopup`) are painted
//! over the rest in the order they came to the front, and panels drawn on top
//! (`SetDrawOnTop`: menus, tooltips) over those. Panels `Remove` marks go at the end of the
//! frame.

use std::cell::{Cell, RefCell};

use gmod_ui::{Clip, Texture};
use mlua::{AnyUserData, IntoLuaMulti, Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::args::{self, Args};
use crate::meta::{self, type_id};
use crate::surface::{color_args, push_color};
use crate::{files, host};

const CACHE: &str = "gmod-lua.panels";

/// A panel: its place among the panels, and which panel there it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelHandle {
    pub index: u32,
    pub serial: u32,
}

/// How a panel is docked in its parent: `NODOCK`, `FILL`, `LEFT`, `RIGHT`, `TOP`, `BOTTOM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Dock {
    #[default]
    None,
    Fill,
    Left,
    Right,
    Top,
    Bottom,
}

impl Dock {
    fn from_number(n: i64) -> Dock {
        match n {
            1 => Dock::Fill,
            2 => Dock::Left,
            3 => Dock::Right,
            4 => Dock::Top,
            5 => Dock::Bottom,
            _ => Dock::None,
        }
    }

    fn number(self) -> i64 {
        match self {
            Dock::None => 0,
            Dock::Fill => 1,
            Dock::Left => 2,
            Dock::Right => 3,
            Dock::Top => 4,
            Dock::Bottom => 5,
        }
    }
}

/// What the engine's classes do beyond a plain panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Panel,
    Label,
    Button,
    TextEntry,
    RichText,
    Html,
    ModelImage,
}

/// The engine's classes, as `vgui.Create` knows them by name.
const CLASSES: &[(&str, Class)] = &[
    ("Panel", Class::Panel),
    ("EditablePanel", Class::Panel),
    ("Frame", Class::Panel),
    ("ImagePanel", Class::Panel),
    ("AvatarImage", Class::Panel),
    ("Label", Class::Label),
    ("URLLabel", Class::Label),
    ("Button", Class::Button),
    ("CheckButton", Class::Button),
    ("ToggleButton", Class::Button),
    ("TextEntry", Class::TextEntry),
    ("RichText", Class::RichText),
    ("HTML", Class::Html),
    ("Awesomium", Class::Html),
    ("Chromium", Class::Html),
    ("ModelImage", Class::ModelImage),
];

/// Text of a `RichText` in one colour.
#[derive(Debug, Clone)]
struct Run {
    text: String,
    color: [u8; 4],
}

#[derive(Debug, Clone)]
struct Node {
    class: Class,
    class_name: &'static str,
    name: String,
    parent: Option<PanelHandle>,
    /// In the order they are drawn when their `ZPos` is the same.
    children: Vec<PanelHandle>,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    min_w: i32,
    min_h: i32,
    visible: bool,
    enabled: bool,
    alpha: u8,
    z: i32,
    dock: Dock,
    /// Left, top, right, bottom.
    margin: [i32; 4],
    padding: [i32; 4],
    /// Wants laying out.
    dirty: bool,
    /// `ApplySchemeSettings` has run.
    schemed: bool,
    /// Whether the panel had `OnChildAdded` when `Prepare` last ran; none before.
    child_hooks: Option<bool>,
    paint_background: bool,
    manual: bool,
    mouse: bool,
    keyboard: bool,
    popup: bool,
    stay_at_back: bool,
    on_top: bool,
    world_clicker: bool,
    modal: bool,
    no_clip: bool,
    fg: [u8; 4],
    bg: [u8; 4],
    cursor: String,
    /// `Remove` was called: it goes at the end of the frame.
    removing: bool,
    text: String,
    font: Option<String>,
    /// Where the text is, as on a number pad: 7 is the top left corner, 5 the middle.
    align: i32,
    inset: (i32, i32),
    wrap: bool,
    shadow: Option<(i32, [u8; 4])>,
    /// Characters before the caret.
    caret: usize,
    /// Where a selection starts; it ends at the caret.
    anchor: Option<usize>,
    multiline: bool,
    max_chars: i32,
    select_all_on_focus: bool,
    runs: Vec<Run>,
    run_color: [u8; 4],
    model: String,
    /// The image of a spawn icon, if the game has one.
    icon: Option<u32>,
    url: String,
}

impl Node {
    fn new(class: Class, class_name: &'static str, name: String) -> Node {
        Node {
            class,
            class_name,
            name,
            parent: None,
            children: Vec::new(),
            x: 0,
            y: 0,
            w: 64,
            h: 24,
            min_w: 0,
            min_h: 0,
            visible: true,
            enabled: true,
            alpha: 255,
            z: 0,
            dock: Dock::None,
            margin: [0; 4],
            padding: [0; 4],
            dirty: true,
            schemed: false,
            child_hooks: None,
            paint_background: true,
            manual: false,
            mouse: true,
            keyboard: true,
            popup: false,
            stay_at_back: false,
            on_top: false,
            world_clicker: false,
            modal: false,
            no_clip: false,
            fg: [255; 4],
            bg: [0; 4],
            cursor: "arrow".to_string(),
            removing: false,
            text: String::new(),
            font: None,
            align: if class == Class::Button { 5 } else { 4 },
            inset: (0, 0),
            wrap: false,
            shadow: None,
            caret: 0,
            anchor: None,
            multiline: false,
            max_chars: 0,
            select_all_on_focus: false,
            runs: Vec::new(),
            run_color: [255; 4],
            model: String::new(),
            icon: None,
            url: String::new(),
        }
    }

    fn has_text(&self) -> bool {
        matches!(self.class, Class::Label | Class::Button | Class::TextEntry | Class::RichText)
    }

    /// The selected characters, first to last.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.caret).then(|| (anchor.min(self.caret), anchor.max(self.caret)))
    }
}

struct Slot {
    serial: u32,
    node: Option<Node>,
}

/// Every panel of a client, and which ones the cursor and the keyboard are at.
#[derive(Default)]
pub(crate) struct Panels {
    slots: RefCell<Vec<Slot>>,
    free: RefCell<Vec<u32>>,
    world: Cell<Option<PanelHandle>>,
    hud: Cell<Option<PanelHandle>>,
    /// Popups, the one in front last.
    popups: RefCell<Vec<PanelHandle>>,
    hovered: Cell<Option<PanelHandle>>,
    capture: Cell<Option<PanelHandle>>,
    focus: Cell<Option<PanelHandle>>,
    /// Marked by `Remove`, to go at the end of the frame.
    doomed: RefCell<Vec<PanelHandle>>,
    /// Being laid out now: `InvalidateLayout(true)` from inside does not start again.
    laying_out: RefCell<Vec<PanelHandle>>,
    clipboard: RefCell<String>,
}

impl Panels {
    fn get<R>(&self, h: PanelHandle, f: impl FnOnce(&Node) -> R) -> Option<R> {
        let slots = self.slots.borrow();
        let slot = slots.get(h.index as usize).filter(|s| s.serial == h.serial)?;
        slot.node.as_ref().map(f)
    }

    fn with<R>(&self, h: PanelHandle, f: impl FnOnce(&mut Node) -> R) -> Option<R> {
        let mut slots = self.slots.borrow_mut();
        let slot = slots.get_mut(h.index as usize).filter(|s| s.serial == h.serial)?;
        slot.node.as_mut().map(f)
    }

    pub fn is_valid(&self, h: PanelHandle) -> bool {
        self.get(h, |_| ()).is_some()
    }

    fn create(&self, node: Node) -> PanelHandle {
        let mut slots = self.slots.borrow_mut();
        match self.free.borrow_mut().pop() {
            Some(index) => {
                let slot = &mut slots[index as usize];
                slot.serial += 1;
                slot.node = Some(node);
                PanelHandle { index, serial: slot.serial }
            }
            None => {
                slots.push(Slot { serial: 1, node: Some(node) });
                PanelHandle { index: slots.len() as u32 - 1, serial: 1 }
            }
        }
    }

    fn children(&self, h: PanelHandle) -> Vec<PanelHandle> {
        self.get(h, |n| n.children.clone()).unwrap_or_default()
    }

    /// The children in the order they are drawn: by `ZPos`, then as they are kept.
    fn drawn_children(&self, h: PanelHandle) -> Vec<PanelHandle> {
        let mut children: Vec<(i32, PanelHandle)> = self.children(h).into_iter().filter_map(|c| self.get(c, |n| (n.z, c))).collect();
        children.sort_by_key(|(z, _)| *z);
        children.into_iter().map(|(_, c)| c).collect()
    }

    fn parent(&self, h: PanelHandle) -> Option<PanelHandle> {
        self.get(h, |n| n.parent).flatten()
    }

    /// Where a panel's top left corner is on the screen.
    fn screen_pos(&self, h: PanelHandle) -> (i32, i32) {
        let (mut x, mut y) = (0, 0);
        let mut at = Some(h);
        for _ in 0..256 {
            let Some(p) = at else { break };
            let Some((px, py, parent)) = self.get(p, |n| (n.x, n.y, n.parent)) else { break };
            x += px;
            y += py;
            at = parent;
        }
        (x, y)
    }

    /// Whether a panel shows: it and its parents are visible, up to the world panel or a
    /// popup.
    fn shown(&self, h: PanelHandle) -> bool {
        let mut at = h;
        for _ in 0..256 {
            let Some((visible, removing, parent)) = self.get(at, |n| (n.visible, n.removing, n.parent)) else { return false };
            if !visible || removing {
                return false;
            }
            match parent {
                Some(p) => at = p,
                None => return Some(at) == self.world.get(),
            }
        }
        false
    }

    /// Whether `ancestor` is `h` or one of its parents.
    fn descends(&self, h: PanelHandle, ancestor: PanelHandle) -> bool {
        let mut at = Some(h);
        for _ in 0..256 {
            match at {
                Some(p) if p == ancestor => return true,
                Some(p) => at = self.parent(p),
                None => return false,
            }
        }
        false
    }

    /// The nearest popup a panel is in, itself included.
    fn popup_of(&self, h: PanelHandle) -> Option<PanelHandle> {
        let mut at = Some(h);
        for _ in 0..256 {
            let p = at?;
            if self.get(p, |n| n.popup)? {
                return Some(p);
            }
            at = self.parent(p);
        }
        None
    }

    fn detach(&self, h: PanelHandle) {
        if let Some(parent) = self.parent(h) {
            self.with(parent, |n| n.children.retain(|c| *c != h));
        }
        self.with(h, |n| n.parent = None);
    }

    fn attach(&self, h: PanelHandle, parent: PanelHandle) {
        self.detach(h);
        self.with(parent, |n| {
            n.children.push(h);
            n.dirty = true;
        });
        self.with(h, |n| n.parent = Some(parent));
    }

    /// Panels drawn on top of everything, as they were made.
    fn on_top(&self) -> Vec<PanelHandle> {
        let slots = self.slots.borrow();
        let mut found = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            if let Some(node) = &slot.node
                && node.on_top
            {
                found.push(PanelHandle { index: index as u32, serial: slot.serial });
            }
        }
        drop(slots);
        found.retain(|h| self.shown(*h));
        found
    }

    fn popups(&self) -> Vec<PanelHandle> {
        let popups = self.popups.borrow().clone();
        popups.into_iter().filter(|p| self.shown(*p) && !self.get(*p, |n| n.on_top).unwrap_or(true)).collect()
    }

    /// Whether the cursor is out: a popup that takes the mouse shows, or the game asked for it.
    pub fn cursor_visible(&self, screen_clicker: bool) -> bool {
        screen_clicker || self.popups().iter().chain(self.on_top().iter()).any(|p| self.get(*p, |n| n.popup && n.mouse).unwrap_or(false))
    }

    /// The panel the keyboard goes to: the one with the focus, if it shows, takes keys, and
    /// so does its popup.
    fn keyboard_target(&self) -> Option<PanelHandle> {
        let focus = self.focus.get()?;
        if !self.shown(focus) || !self.get(focus, |n| n.keyboard)? {
            return None;
        }
        let popup = self.popup_of(focus)?;
        self.get(popup, |n| n.keyboard)?.then_some(focus)
    }

    /// The panel at a point of the screen that takes the mouse.
    fn hit(&self, h: PanelHandle, origin: (i32, i32), point: (i32, i32), clip: Clip) -> Option<PanelHandle> {
        let (visible, mouse, x, y, w, hh, no_clip) = self.get(h, |n| (n.visible && !n.removing, n.mouse, n.x, n.y, n.w, n.h, n.no_clip))?;
        if !visible || !mouse {
            return None;
        }
        let (sx, sy) = (origin.0 + x, origin.1 + y);
        let own = Clip::new(sx, sy, w, hh);
        let clip = if no_clip { clip } else { clip.intersect(own) };
        let inside = |c: &Clip| point.0 >= c.left && point.0 < c.right && point.1 >= c.top && point.1 < c.bottom;
        if !no_clip && !inside(&clip) {
            return None;
        }
        for child in self.drawn_children(h).into_iter().rev() {
            if self.get(child, |n| n.popup || n.on_top).unwrap_or(true) {
                continue;
            }
            if let Some(found) = self.hit(child, (sx, sy), point, clip) {
                return Some(found);
            }
        }
        inside(&clip.intersect(own)).then_some(h)
    }

    /// The panel under the cursor: those on top first, then the popups, then the rest.
    fn under(&self, point: (i32, i32), screen: Clip) -> Option<PanelHandle> {
        let layers: Vec<PanelHandle> = self.on_top().into_iter().rev().chain(self.popups().into_iter().rev()).collect();
        for p in layers {
            let parent = self.parent(p);
            let origin = parent.map_or((0, 0), |parent| self.screen_pos(parent));
            if let Some(found) = self.hit(p, origin, point, screen) {
                return Some(found);
            }
        }
        self.hit(self.world.get()?, (0, 0), point, screen)
    }
}

impl UserData for PanelHandle {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("MetaName", "Panel");
        fields.add_meta_field("MetaID", type_id::PANEL);
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function("__index", |lua, (ud, key): (AnyUserData, Value)| index(lua, &ud, key));
        methods.add_meta_function("__newindex", |lua, (ud, key, value): (AnyUserData, Value, Value)| -> mlua::Result<()> {
            let h = *ud.borrow::<PanelHandle>()?;
            if let Value::String(s) = &key {
                let number = match &value {
                    Value::Integer(n) => Some(*n as i32),
                    Value::Number(n) => Some(*n as i32),
                    _ => None,
                };
                match (s.as_bytes().as_ref(), number) {
                    (b"x", Some(x)) => {
                        set_pos(lua, h, Some(x), None);
                        return Ok(());
                    }
                    (b"y", Some(y)) => {
                        set_pos(lua, h, None, Some(y));
                        return Ok(());
                    }
                    _ => {}
                }
            }
            match table_of(&ud)? {
                Some(table) => table.set(key, value),
                None => Ok(()),
            }
        });
        methods.add_meta_function("__eq", |_, (a, b): (AnyUserData, AnyUserData)| {
            Ok(matches!((a.borrow::<PanelHandle>(), b.borrow::<PanelHandle>()), (Ok(a), Ok(b)) if *a == *b))
        });
        methods.add_meta_function("__tostring", |lua, ud: AnyUserData| {
            let h = *ud.borrow::<PanelHandle>()?;
            Ok(host(lua)
                .panels
                .get(h, |n| format!("Panel: [name:{}][class:{}][{},{},{},{}]", n.name, n.class_name, n.x, n.y, n.w, n.h))
                .unwrap_or_else(|| "[NULL Panel]".to_string()))
        });
    }
}

/// `panel.key`: `x` and `y` are where it is; then the panel's table, then the metatable.
fn index(lua: &Lua, ud: &AnyUserData, key: Value) -> mlua::Result<Value> {
    let h = *ud.borrow::<PanelHandle>()?;
    if let Value::String(s) = &key {
        match s.as_bytes().as_ref() {
            b"x" => return Ok(host(lua).panels.get(h, |n| Value::Integer(n.x.into())).unwrap_or(Value::Nil)),
            b"y" => return Ok(host(lua).panels.get(h, |n| Value::Integer(n.y.into())).unwrap_or(Value::Nil)),
            _ => {}
        }
    }
    if let Some(table) = table_of(ud)? {
        let value: Value = table.get(key.clone())?;
        if !value.is_nil() {
            return Ok(value);
        }
    }
    meta::lookup(lua, "Panel", key)
}

fn table_of(ud: &AnyUserData) -> mlua::Result<Option<Table>> {
    ud.named_user_value::<Option<Table>>("table")
}

/// The userdata of a panel, the same one each time; `nil` for one that is gone.
pub(crate) fn push(lua: &Lua, h: PanelHandle) -> mlua::Result<Value> {
    if !host(lua).panels.is_valid(h) {
        return Ok(Value::Nil);
    }
    let cache: Table = lua.named_registry_value(CACHE)?;
    if let Some(ud) = cache.raw_get::<Option<AnyUserData>>(h.index)?
        && *ud.borrow::<PanelHandle>()? == h
    {
        return Ok(Value::UserData(ud));
    }
    let ud = lua.create_userdata(h)?;
    ud.set_named_user_value("table", lua.create_table()?)?;
    cache.raw_set(h.index, &ud)?;
    Ok(Value::UserData(ud))
}

fn push_opt(lua: &Lua, h: Option<PanelHandle>) -> mlua::Result<Value> {
    match h {
        Some(h) => push(lua, h),
        None => Ok(Value::Nil),
    }
}

/// Calls a panel's function `name` (from its table or the metatable) with the panel first.
/// None when it has none, or it failed (the error is reported).
fn call(lua: &Lua, h: PanelHandle, name: &str, args: impl IntoLuaMulti) -> Option<MultiValue> {
    let Ok(Value::UserData(ud)) = push(lua, h) else { return None };
    let Ok(Value::Function(f)) = index(lua, &ud, Value::String(lua.create_string(name).ok()?)) else { return None };
    let mut values = MultiValue::from_iter([Value::UserData(ud)]);
    values.extend(args.into_lua_multi(lua).ok()?);
    files::call_reporting(lua, f, values).ok()
}

/// Whether a panel has a function `name`.
fn has(lua: &Lua, h: PanelHandle, name: &str) -> bool {
    let Ok(Value::UserData(ud)) = push(lua, h) else { return false };
    let Ok(key) = lua.create_string(name) else { return false };
    matches!(index(lua, &ud, Value::String(key)), Ok(Value::Function(_)))
}

fn returned_true(result: &Option<MultiValue>) -> bool {
    matches!(result.as_ref().and_then(|r| r.front()), Some(Value::Boolean(true)))
}

/// Sets a field of a panel's table.
fn set_field(lua: &Lua, h: PanelHandle, key: &str, value: impl mlua::IntoLua) {
    if let Ok(Value::UserData(ud)) = push(lua, h)
        && let Ok(Some(table)) = table_of(&ud)
    {
        let _ = table.raw_set(key, value);
    }
}

/// Calls `name` on a panel, or on the first of its parents that has it.
fn bubble(lua: &Lua, h: PanelHandle, name: &str, args: impl IntoLuaMulti + Clone) -> Option<MultiValue> {
    let panels = &host(lua).panels;
    let world = panels.world.get();
    let mut at = Some(h);
    for _ in 0..256 {
        let p = at?;
        if Some(p) == world {
            return None;
        }
        if has(lua, p, name) {
            return call(lua, p, name, args);
        }
        at = panels.parent(p);
    }
    None
}

// --- changes the engine makes on its own ------------------------------------------------

fn set_pos(lua: &Lua, h: PanelHandle, x: Option<i32>, y: Option<i32>) {
    host(lua).panels.with(h, |n| {
        n.x = x.unwrap_or(n.x);
        n.y = y.unwrap_or(n.y);
    });
}

/// A new size: the panel is laid out again, and so is its parent when it is docked there.
fn set_size(lua: &Lua, h: PanelHandle, w: i32, hh: i32) {
    let panels = &host(lua).panels;
    let changed = panels.with(h, |n| {
        let (w, hh) = (w.max(n.min_w).max(0), hh.max(n.min_h).max(0));
        if (w, hh) == (n.w, n.h) {
            return None;
        }
        n.w = w;
        n.h = hh;
        n.dirty = true;
        Some((n.dock != Dock::None, n.parent))
    });
    if let Some(Some((docked, parent))) = changed {
        if docked && let Some(parent) = parent {
            panels.with(parent, |n| n.dirty = true);
        }
        let size = panels.get(h, |n| (n.w, n.h)).unwrap_or_default();
        call(lua, h, "OnSizeChanged", size);
    }
}

fn set_parent(lua: &Lua, h: PanelHandle, parent: Option<PanelHandle>) {
    let panels = &host(lua).panels;
    let old = panels.parent(h);
    if old == parent || parent.is_some_and(|p| panels.descends(p, h)) {
        return;
    }
    match parent {
        Some(parent) => panels.attach(h, parent),
        None => panels.detach(h),
    }
    if let Some(old) = old {
        panels.with(old, |n| n.dirty = true);
        if hears_children(lua, old)
            && let Ok(child) = push(lua, h)
        {
            call(lua, old, "OnChildRemoved", child);
        }
    }
    if let Some(parent) = parent
        && hears_children(lua, parent)
        && let Ok(child) = push(lua, h)
    {
        call(lua, parent, "OnChildAdded", child);
    }
}

/// Whether a panel is told of children coming and going. `Prepare`, which `vgui.Create`
/// calls after each class's `Init`, sets up the functions the engine calls as the panel's
/// table has them then: while `DScrollPanel`'s `Init` makes its canvas and scroll bar, its
/// `OnChildAdded` is not set up yet (the base class's `Prepare` ran without it), and they stay
/// its own children; what is added after goes into the canvas. A panel never prepared (made
/// with `vgui.CreateX`) has its functions looked up each time.
fn hears_children(lua: &Lua, h: PanelHandle) -> bool {
    match host(lua).panels.get(h, |n| n.child_hooks) {
        Some(Some(hooks)) => hooks,
        Some(None) => true,
        None => false,
    }
}

fn set_focus(lua: &Lua, h: Option<PanelHandle>) {
    let panels = &host(lua).panels;
    let old = panels.focus.get();
    if old == h {
        return;
    }
    panels.focus.set(h);
    if let Some(old) = old.filter(|o| panels.is_valid(*o)) {
        call(lua, old, "OnLoseFocus", ());
        call(lua, old, "OnFocusChanged", false);
    }
    if let Some(h) = h {
        let select_all = panels.with(h, |n| {
            if n.select_all_on_focus {
                n.anchor = Some(0);
                n.caret = n.text.chars().count();
            }
            n.select_all_on_focus
        });
        let _ = select_all;
        call(lua, h, "OnGetFocus", ());
        call(lua, h, "OnFocusChanged", true);
    }
}

fn move_to_front(lua: &Lua, h: PanelHandle) {
    let panels = &host(lua).panels;
    if let Some(parent) = panels.parent(h) {
        panels.with(parent, |n| {
            n.children.retain(|c| *c != h);
            n.children.push(h);
        });
    }
    if panels.get(h, |n| n.popup && !n.stay_at_back).unwrap_or(false) {
        let mut popups = panels.popups.borrow_mut();
        popups.retain(|p| *p != h);
        popups.push(h);
    }
}

/// Marks a panel to go at the end of the frame.
fn remove(lua: &Lua, h: PanelHandle) {
    let panels = &host(lua).panels;
    if Some(h) == panels.world.get() || Some(h) == panels.hud.get() {
        return;
    }
    if panels.with(h, |n| std::mem::replace(&mut n.removing, true)) == Some(false) {
        panels.doomed.borrow_mut().push(h);
    }
}

/// Deletes a panel and those in it: `OnRemove` runs for each.
fn delete(lua: &Lua, h: PanelHandle) {
    let panels = &host(lua).panels;
    if !panels.is_valid(h) {
        return;
    }
    call(lua, h, "OnRemove", ());
    for child in panels.children(h) {
        delete(lua, child);
    }
    if let Some(parent) = panels.parent(h) {
        panels.detach(h);
        panels.with(parent, |n| n.dirty = true);
        if hears_children(lua, parent)
            && let Ok(child) = push(lua, h)
        {
            call(lua, parent, "OnChildRemoved", child);
        }
    }
    if let Ok(cache) = lua.named_registry_value::<Table>(CACHE) {
        let _ = cache.raw_set(h.index, Value::Nil);
    }
    {
        let mut slots = panels.slots.borrow_mut();
        if let Some(slot) = slots.get_mut(h.index as usize).filter(|s| s.serial == h.serial) {
            slot.node = None;
            panels.free.borrow_mut().push(h.index);
        }
    }
    panels.popups.borrow_mut().retain(|p| *p != h);
    for cell in [&panels.hovered, &panels.capture, &panels.focus] {
        if cell.get() == Some(h) {
            cell.set(None);
        }
    }
}

// --- each frame ----------------------------------------------------------------------------

/// The world panel and the HUD's are the size of the screen.
fn fit_roots(lua: &Lua) {
    let host = host(lua);
    let (w, h) = host.screen.size.get();
    for root in [host.panels.world.get(), host.panels.hud.get()].into_iter().flatten() {
        set_size(lua, root, w as i32, h as i32);
    }
}

/// Every panel that shows, parents before children: popups and those on top included.
fn shown_panels(lua: &Lua) -> Vec<PanelHandle> {
    let panels = &host(lua).panels;
    let mut out = Vec::new();
    let mut stack: Vec<PanelHandle> = panels.world.get().into_iter().collect();
    while let Some(h) = stack.pop() {
        if !panels.get(h, |n| n.visible && !n.removing).unwrap_or(false) {
            continue;
        }
        out.push(h);
        stack.extend(panels.children(h).into_iter().rev());
    }
    out
}

/// `Think` and `AnimationThink` of every panel that shows.
pub(crate) fn think(lua: &Lua) {
    for h in shown_panels(lua) {
        if host(lua).panels.is_valid(h) {
            call(lua, h, "AnimationThink", ());
            call(lua, h, "Think", ());
        }
    }
}

/// Lays out a panel now: its docked children, then its `PerformLayout`.
fn perform_layout(lua: &Lua, h: PanelHandle) {
    let panels = &host(lua).panels;
    if panels.laying_out.borrow().contains(&h) || !panels.is_valid(h) {
        return;
    }
    panels.laying_out.borrow_mut().push(h);
    let first = panels.with(h, |n| {
        n.dirty = false;
        !std::mem::replace(&mut n.schemed, true)
    });
    if first == Some(true) {
        call(lua, h, "ApplySchemeSettings", ());
    }
    dock_children(lua, h);
    if let Some(size) = panels.get(h, |n| (n.w, n.h)) {
        call(lua, h, "PerformLayout", size);
    }
    panels.laying_out.borrow_mut().retain(|p| *p != h);
}

/// Places the docked children inside the padding: those docked to a side first, in order,
/// each taking its edge of what is left; then those that fill what is left.
fn dock_children(lua: &Lua, h: PanelHandle) {
    let panels = &host(lua).panels;
    let Some((w, hh, padding, children)) = panels.get(h, |n| (n.w, n.h, n.padding, n.children.clone())) else { return };
    let (mut left, mut top, mut right, mut bottom) = (padding[0], padding[1], w - padding[2], hh - padding[3]);
    let mut resized = Vec::new();
    for fill in [false, true] {
        for &child in &children {
            let Some((dock, m, cw, ch, visible)) = panels.get(child, |n| (n.dock, n.margin, n.w, n.h, n.visible && !n.removing)) else {
                continue;
            };
            if !visible || dock == Dock::None || (dock == Dock::Fill) != fill {
                continue;
            }
            let (x, y, nw, nh) = match dock {
                Dock::Top => {
                    let r = (left + m[0], top + m[1], right - left - m[0] - m[2], ch);
                    top += ch + m[1] + m[3];
                    r
                }
                Dock::Bottom => {
                    let r = (left + m[0], bottom - ch - m[3], right - left - m[0] - m[2], ch);
                    bottom -= ch + m[1] + m[3];
                    r
                }
                Dock::Left => {
                    let r = (left + m[0], top + m[1], cw, bottom - top - m[1] - m[3]);
                    left += cw + m[0] + m[2];
                    r
                }
                Dock::Right => {
                    let r = (right - cw - m[2], top + m[1], cw, bottom - top - m[1] - m[3]);
                    right -= cw + m[0] + m[2];
                    r
                }
                Dock::Fill => (left + m[0], top + m[1], right - left - m[0] - m[2], bottom - top - m[1] - m[3]),
                Dock::None => continue,
            };
            let changed = panels.with(child, |n| {
                n.x = x;
                n.y = y;
                let (nw, nh) = (nw.max(n.min_w).max(0), nh.max(n.min_h).max(0));
                let changed = (nw, nh) != (n.w, n.h);
                n.w = nw;
                n.h = nh;
                n.dirty |= changed;
                changed
            });
            if changed == Some(true) {
                resized.push(child);
            }
        }
    }
    for child in resized {
        let size = panels.get(child, |n| (n.w, n.h)).unwrap_or_default();
        call(lua, child, "OnSizeChanged", size);
    }
}

fn layout_tree(lua: &Lua, h: PanelHandle, depth: u32) {
    let panels = &host(lua).panels;
    let Some((visible, dirty)) = panels.get(h, |n| (n.visible && !n.removing, n.dirty)) else { return };
    if !visible || depth > 128 {
        return;
    }
    if dirty {
        perform_layout(lua, h);
    }
    for child in panels.children(h) {
        layout_tree(lua, child, depth + 1);
    }
}

/// Lays out what wants it, from the world panel down.
pub(crate) fn layout(lua: &Lua) {
    fit_roots(lua);
    if let Some(world) = host(lua).panels.world.get() {
        layout_tree(lua, world, 0);
    }
}

/// Where drawing goes while a panel paints: its corner, its rectangle, its alpha.
fn enter(lua: &Lua, origin: (i32, i32), clip: Clip, alpha: f32) {
    let screen = &host(lua).screen;
    screen.canvas.borrow_mut().origin = [origin.0 as f32, origin.1 as f32];
    screen.panel_clip.set(Some(clip));
    screen.set_alpha(alpha);
    screen.update_clip();
}

fn leave(lua: &Lua) {
    let screen = &host(lua).screen;
    screen.canvas.borrow_mut().origin = [0.0; 2];
    screen.panel_clip.set(None);
    screen.set_alpha(1.0);
    screen.update_clip();
}

fn screen_clip(lua: &Lua) -> Clip {
    let (w, h) = host(lua).screen.size.get();
    Clip::new(0, 0, w as i32, h as i32)
}

/// Paints a panel and what is in it, its parent's corner at `origin`.
fn paint_panel(lua: &Lua, h: PanelHandle, origin: (i32, i32), clip: Clip, alpha: f32, depth: u32) {
    let panels = &host(lua).panels;
    let Some((visible, x, y, w, hh, no_clip, own_alpha, background, bg)) =
        panels.get(h, |n| (n.visible && !n.removing, n.x, n.y, n.w, n.h, n.no_clip, n.alpha, n.paint_background, n.bg))
    else {
        return;
    };
    if !visible || depth > 128 {
        return;
    }
    let corner = (origin.0 + x, origin.1 + y);
    let own = Clip::new(corner.0, corner.1, w, hh);
    let clip = if no_clip { screen_clip(lua) } else { clip.intersect(own) };
    let alpha = alpha * f32::from(own_alpha) / 255.0;
    if clip.is_empty() || alpha <= 0.0 {
        return;
    }
    enter(lua, corner, clip, alpha);
    if background && bg[3] > 0 {
        let screen = &host(lua).screen;
        let color = screen.color(bg);
        screen.canvas.borrow_mut().rect(0.0, 0.0, w as f32, hh as f32, color);
    }
    let painted = has(lua, h, "Paint");
    if painted {
        call(lua, h, "Paint", (w, hh));
        enter(lua, corner, clip, alpha);
    }
    paint_own(lua, h, painted);
    for child in panels.drawn_children(h) {
        if panels.get(child, |n| n.popup || n.on_top || n.manual).unwrap_or(true) {
            continue;
        }
        paint_panel(lua, child, corner, clip, alpha, depth + 1);
    }
    if has(lua, h, "PaintOver") {
        enter(lua, corner, clip, alpha);
        call(lua, h, "PaintOver", (w, hh));
    }
}

/// Paints the panels: the world panel's, then the popups, then those on top.
pub(crate) fn paint(lua: &Lua) {
    let panels = &host(lua).panels;
    let screen = screen_clip(lua);
    if let Some(world) = panels.world.get() {
        paint_panel(lua, world, (0, 0), screen, 1.0, 0);
    }
    for p in panels.popups().into_iter().chain(panels.on_top()) {
        if panels.get(p, |n| n.manual).unwrap_or(true) {
            continue;
        }
        let origin = panels.parent(p).map_or((0, 0), |parent| panels.screen_pos(parent));
        paint_panel(lua, p, origin, screen, 1.0, 0);
    }
    leave(lua);
}

/// Removes the panels marked for it.
pub(crate) fn remove_doomed(lua: &Lua) {
    for _ in 0..16 {
        let doomed = std::mem::take(&mut *host(lua).panels.doomed.borrow_mut());
        if doomed.is_empty() {
            break;
        }
        for h in doomed {
            delete(lua, h);
        }
    }
}

// --- the engine's own drawing -----------------------------------------------------------

/// The font of a panel's text, `Default` when it has none it knows.
fn font_of(lua: &Lua, name: Option<&str>) -> Option<gmod_ui::FontId> {
    let screen = &host(lua).screen;
    name.and_then(|n| screen.font(n)).or_else(|| screen.font("Default"))
}

fn text_width(lua: &Lua, font: gmod_ui::FontId, text: &str) -> i32 {
    host(lua).screen.fonts.borrow_mut().text_size(font, text).0
}

/// Lines of text that fit `width`, broken between words where they can be.
fn wrap(lua: &Lua, font: gmod_ui::FontId, text: &str, width: i32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_inclusive(' ') {
            let candidate = format!("{line}{word}");
            if line.is_empty() || text_width(lua, font, candidate.trim_end()) <= width {
                line = candidate;
                continue;
            }
            lines.push(std::mem::take(&mut line).trim_end().to_string());
            line = word.to_string();
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

/// What a label shows: its text, localised.
fn shown_text(lua: &Lua, text: &str) -> String {
    if text.starts_with('#') { host(lua).client.phrase(text) } else { text.to_string() }
}

/// Lines of text, each where it goes.
type Lines = Vec<(String, i32, i32)>;

/// The text of a label in its rectangle, aligned as it says.
fn label_lines(lua: &Lua, h: PanelHandle) -> Option<(gmod_ui::FontId, Lines)> {
    let (text, font, align, inset, wrapped, w, hh) =
        host(lua).panels.get(h, |n| (n.text.clone(), n.font.clone(), n.align, n.inset, n.wrap, n.w, n.h))?;
    let font = font_of(lua, font.as_deref())?;
    let text = shown_text(lua, &text);
    let tall = host(lua).screen.fonts.borrow().tall(font);
    let lines = if wrapped { wrap(lua, font, &text, w - inset.0) } else { text.split('\n').map(str::to_string).collect() };
    let total = tall * lines.len() as i32;
    let top = match align {
        7..=9 => 0,
        1..=3 => hh - total,
        _ => (hh - total) / 2,
    } + inset.1;
    let placed = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let lw = text_width(lua, font, &line);
            let x = match align {
                7 | 4 | 1 => 0,
                9 | 6 | 3 => w - lw,
                _ => (w - lw) / 2,
            } + inset.0;
            (line, x, top + i as i32 * tall)
        })
        .collect();
    Some((font, placed))
}

/// What the engine draws of a panel after its `Paint`: a label's text, a rich text's lines,
/// a spawn icon's image, a text entry's text (when Lua does not paint it).
fn paint_own(lua: &Lua, h: PanelHandle, painted: bool) {
    let Some((class, fg, shadow, w, hh, icon)) = host(lua).panels.get(h, |n| (n.class, n.fg, n.shadow, n.w, n.h, n.icon)) else { return };
    let screen = &host(lua).screen;
    match class {
        Class::Label | Class::Button => {
            let Some((font, lines)) = label_lines(lua, h) else { return };
            for (line, x, y) in lines {
                if let Some((distance, color)) = shadow {
                    screen.text(font, (x + distance) as f32, (y + distance) as f32, &line, color);
                }
                screen.text(font, x as f32, y as f32, &line, fg);
            }
        }
        Class::TextEntry if !painted => draw_entry(lua, h, fg, [0, 120, 215, 255], fg),
        Class::RichText => paint_rich(lua, h),
        Class::ModelImage => {
            if let Some(id) = icon {
                let color = screen.color([255; 4]);
                screen.canvas.borrow_mut().image(Texture::Image(id), false, [0.0, 0.0, w as f32, hh as f32], [0.0, 0.0, 1.0, 1.0], color);
            }
        }
        _ => {}
    }
}

/// A text entry's text, selection and caret, scrolled so the caret shows.
fn draw_entry(lua: &Lua, h: PanelHandle, text_color: [u8; 4], highlight: [u8; 4], caret_color: [u8; 4]) {
    let panels = &host(lua).panels;
    let Some((text, font, caret, selection, multiline, w, hh)) =
        panels.get(h, |n| (n.text.clone(), n.font.clone(), n.caret, n.selection(), n.multiline, n.w, n.h))
    else {
        return;
    };
    let Some(font) = font_of(lua, font.as_deref()) else { return };
    let screen = &host(lua).screen;
    let tall = screen.fonts.borrow().tall(font);
    let focused = panels.focus.get() == Some(h);
    let blink = ((host(lua).started.elapsed().as_secs_f64() * 2.0) as u64).is_multiple_of(2);
    // lines with the index of their first character
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        lines.push((start, line));
        start += line.chars().count() + 1;
    }
    if !multiline {
        lines.truncate(1);
    }
    let caret_line = lines.iter().rposition(|(s, _)| *s <= caret).unwrap_or(0);
    let prefix_width = |line: &str, chars: usize| -> i32 {
        let prefix: String = line.chars().take(chars).collect();
        text_width(lua, font, &prefix)
    };
    let caret_x = prefix_width(lines[caret_line].1, caret - lines[caret_line].0);
    let scroll = (caret_x - (w - 6)).max(0);
    let first_y = if multiline { 3 } else { (hh - tall) / 2 };
    let scroll_y = if multiline { ((caret_line as i32 + 1) * tall - (hh - 6)).max(0) } else { 0 };
    for (i, (begin, line)) in lines.iter().enumerate() {
        let y = first_y + i as i32 * tall - scroll_y;
        let count = line.chars().count();
        if let Some((from, to)) = selection {
            let (a, b) = (from.clamp(*begin, begin + count), to.clamp(*begin, begin + count));
            if b > a {
                let (x0, x1) = (prefix_width(line, a - begin), prefix_width(line, b - begin));
                let color = screen.color(highlight);
                screen.canvas.borrow_mut().rect((3 + x0 - scroll) as f32, y as f32, (x1 - x0) as f32, tall as f32, color);
            }
        }
        screen.text(font, (3 - scroll) as f32, y as f32, line, text_color);
    }
    if focused && blink {
        let color = screen.color(caret_color);
        let y = first_y + caret_line as i32 * tall - scroll_y;
        screen.canvas.borrow_mut().rect((3 + caret_x - scroll) as f32, y as f32, 1.0, tall as f32, color);
    }
}

/// The lines of a rich text, wrapped to its width, the last ones showing.
fn paint_rich(lua: &Lua, h: PanelHandle) {
    let Some((runs, font, w, hh)) = host(lua).panels.get(h, |n| (n.runs.clone(), n.font.clone(), n.w, n.h)) else { return };
    let Some(font) = font_of(lua, font.as_deref()) else { return };
    let lines = rich_lines(lua, font, &runs, w - 4);
    let screen = &host(lua).screen;
    let tall = screen.fonts.borrow().tall(font);
    let shown = (hh / tall.max(1)).max(1) as usize;
    let skip = lines.len().saturating_sub(shown);
    for (i, line) in lines.iter().skip(skip).enumerate() {
        let mut x = 2.0;
        for (text, color) in line {
            x += screen.text(font, x, (i as i32 * tall) as f32, text, *color);
        }
    }
}

type RichLine = Vec<(String, [u8; 4])>;

fn rich_lines(lua: &Lua, font: gmod_ui::FontId, runs: &[Run], width: i32) -> Vec<RichLine> {
    let mut lines: Vec<RichLine> = vec![Vec::new()];
    let mut x = 0;
    for run in runs {
        for (i, part) in run.text.split('\n').enumerate() {
            if i > 0 {
                lines.push(Vec::new());
                x = 0;
            }
            for word in part.split_inclusive(' ') {
                let ww = text_width(lua, font, word);
                if x > 0 && x + ww > width {
                    lines.push(Vec::new());
                    x = 0;
                }
                let line = lines.last_mut().expect("never empty");
                match line.last_mut() {
                    Some((text, color)) if *color == run.color => text.push_str(word),
                    _ => line.push((word.to_string(), run.color)),
                }
                x += ww;
            }
        }
    }
    lines
}

// --- input ---------------------------------------------------------------------------------

/// The cursor is somewhere new: the panel under it is hovered, and the one that has the
/// mouse hears where it went.
pub(crate) fn cursor_moved(lua: &Lua) {
    let host = host(lua);
    let visible = host.panels.cursor_visible(host.client.screen_clicker.get());
    let point = host.client.cursor.get();
    let under = if visible { host.panels.under(point, screen_clip(lua)) } else { None };
    let old = host.panels.hovered.get();
    if old != under {
        host.panels.hovered.set(under);
        if let Some(old) = old.filter(|o| host.panels.is_valid(*o)) {
            set_field(lua, old, "Hovered", false);
            call(lua, old, "OnCursorExited", ());
        }
        if let Some(new) = under {
            set_field(lua, new, "Hovered", true);
            call(lua, new, "OnCursorEntered", ());
        }
    }
    if let Some(target) = host.panels.capture.get().or(under) {
        let (sx, sy) = host.panels.screen_pos(target);
        call(lua, target, "OnCursorMoved", (point.0 - sx, point.1 - sy));
    }
}

/// Whether the panel is the world (or lets clicks through to it).
fn is_world(lua: &Lua, h: PanelHandle) -> bool {
    let panels = &host(lua).panels;
    Some(h) == panels.world.get() || Some(h) == panels.hud.get() || panels.get(h, |n| n.world_clicker).unwrap_or(false)
}

/// A mouse button went down or up (`MOUSE_*`). True when the panels took it, so the game
/// does not.
pub(crate) fn mouse_button(lua: &Lua, code: i32, down: bool) -> bool {
    let host = host(lua);
    if !host.panels.cursor_visible(host.client.screen_clicker.get()) {
        return false;
    }
    let Some(target) = host.panels.capture.get().or(host.panels.hovered.get()) else { return false };
    if is_world(lua, target) {
        let point = host.client.cursor.get();
        let aim = crate::client::screen_to_vector(lua, point.0 as f32, point.1 as f32);
        let aim = Value::UserData(match lua.create_userdata(aim) {
            Ok(ud) => ud,
            Err(_) => return true,
        });
        let hook = if down { "GUIMousePressed" } else { "GUIMouseReleased" };
        crate::call_hook(lua, hook, MultiValue::from_iter([Value::Integer(code.into()), aim]));
        if down && let Ok(panel) = push(lua, target) {
            crate::call_hook(lua, "VGUIMousePressed", MultiValue::from_iter([panel, Value::Integer(code.into())]));
        }
        return true;
    }
    if down {
        if let Ok(panel) = push(lua, target) {
            crate::call_hook(lua, "VGUIMousePressed", MultiValue::from_iter([panel, Value::Integer(code.into())]));
        }
        if !host.panels.is_valid(target) {
            return true;
        }
        if let Some(popup) = host.panels.popup_of(target) {
            move_to_front(lua, popup);
        }
        let takes_keys = host.panels.get(target, |n| n.keyboard && n.class == Class::TextEntry).unwrap_or(false);
        if takes_keys {
            set_focus(lua, Some(target));
        } else if host.panels.focus.get().is_some_and(|f| host.panels.get(f, |n| n.class == Class::TextEntry).unwrap_or(false)) {
            set_focus(lua, None);
        }
        bubble(lua, target, "OnMousePressed", code);
    } else {
        bubble(lua, target, "OnMouseReleased", code);
        let clicked = host.panels.get(target, |n| n.class == Class::Button && n.enabled).unwrap_or(false);
        if clicked && code == MOUSE_LEFT && host.panels.hovered.get() == Some(target) {
            call(lua, target, "DoClick", ());
        }
    }
    true
}

const MOUSE_LEFT: i32 = 107;

/// The mouse wheel turned: the panel under the cursor or the first of its parents that
/// scrolls takes it.
pub(crate) fn mouse_wheel(lua: &Lua, delta: i32) -> bool {
    let host = host(lua);
    if !host.panels.cursor_visible(host.client.screen_clicker.get()) {
        return false;
    }
    let mut at = host.panels.hovered.get();
    for _ in 0..256 {
        let Some(h) = at else { break };
        if is_world(lua, h) {
            break;
        }
        if returned_true(&call(lua, h, "OnMouseWheeled", delta)) {
            return true;
        }
        at = host.panels.parent(h);
    }
    host.panels.hovered.get().is_some_and(|h| !is_world(lua, h))
}

const KEY_A: i32 = 11;
const KEY_C: i32 = 13;
const KEY_V: i32 = 32;
const KEY_X: i32 = 34;
const KEY_PAD_ENTER: i32 = 51;
const KEY_ENTER: i32 = 64;
const KEY_BACKSPACE: i32 = 66;
const KEY_DELETE: i32 = 73;
const KEY_HOME: i32 = 74;
const KEY_END: i32 = 75;
const KEY_UP: i32 = 88;
const KEY_LEFT: i32 = 89;
const KEY_DOWN: i32 = 90;
const KEY_RIGHT: i32 = 91;

/// A key went down or up (`KEY_*`). True when a panel has the keyboard, so the game does
/// not get it.
pub(crate) fn key(lua: &Lua, code: i32, down: bool) -> bool {
    let panels = &host(lua).panels;
    let Some(target) = panels.keyboard_target() else { return false };
    if !down {
        bubble(lua, target, "OnKeyCodeReleased", code);
        return true;
    }
    bubble(lua, target, "OnKeyCodePressed", code);
    if panels.get(target, |n| n.class == Class::TextEntry).unwrap_or(false) && !returned_true(&call(lua, target, "OnKeyCodeTyped", code)) {
        edit_key(lua, target, code);
    }
    true
}

/// Text was typed: it goes into the text entry that has the keyboard.
pub(crate) fn text_typed(lua: &Lua, text: &str) -> bool {
    let panels = &host(lua).panels;
    let Some(target) = panels.keyboard_target() else { return false };
    if !panels.get(target, |n| n.class == Class::TextEntry && n.enabled).unwrap_or(false) {
        return true;
    }
    for ch in text.chars().filter(|c| !c.is_control()) {
        if returned_true(&call(lua, target, "AllowInput", ch.to_string())) {
            continue;
        }
        insert(lua, target, &ch.to_string());
    }
    true
}

/// Puts text where the caret is, over the selection.
fn insert(lua: &Lua, h: PanelHandle, text: &str) {
    let changed = host(lua).panels.with(h, |n| {
        let mut chars: Vec<char> = n.text.chars().collect();
        if let Some((from, to)) = n.selection() {
            chars.drain(from..to);
            n.caret = from;
        }
        n.anchor = None;
        let mut added: Vec<char> = text.chars().filter(|c| *c != '\n' || n.multiline).collect();
        if n.max_chars > 0 {
            added.truncate((n.max_chars as usize).saturating_sub(chars.len()));
        }
        if added.is_empty() {
            return false;
        }
        let at = n.caret.min(chars.len());
        chars.splice(at..at, added.iter().copied());
        n.caret = at + added.len();
        n.text = chars.into_iter().collect();
        true
    });
    if changed == Some(true) {
        call(lua, h, "OnTextChanged", ());
    }
}

/// What a key does in a text entry: moving the caret, selecting, deleting, the clipboard.
fn edit_key(lua: &Lua, h: PanelHandle, code: i32) {
    let host = host(lua);
    let shift = host.client.is_down(79) || host.client.is_down(80);
    let control = host.client.is_down(83) || host.client.is_down(84);
    if control && code == KEY_A {
        host.panels.with(h, |n| {
            n.anchor = Some(0);
            n.caret = n.text.chars().count();
        });
        return;
    }
    if control && (code == KEY_C || code == KEY_X) {
        let selected = host.panels.get(h, |n| n.selection().map(|(a, b)| n.text.chars().skip(a).take(b - a).collect::<String>()));
        if let Some(Some(text)) = selected {
            *host.panels.clipboard.borrow_mut() = text;
            if code == KEY_X {
                insert_or_delete(lua, h, None);
            }
        }
        return;
    }
    if control && code == KEY_V {
        let text = host.panels.clipboard.borrow().clone();
        insert(lua, h, &text);
        return;
    }
    match code {
        KEY_BACKSPACE => insert_or_delete(lua, h, Some(-1)),
        KEY_DELETE => insert_or_delete(lua, h, Some(1)),
        KEY_ENTER | KEY_PAD_ENTER => {
            if host.panels.get(h, |n| n.multiline).unwrap_or(false) {
                insert(lua, h, "\n");
            } else {
                call(lua, h, "OnEnter", ());
            }
        }
        KEY_LEFT | KEY_RIGHT | KEY_HOME | KEY_END | KEY_UP | KEY_DOWN => {
            host.panels.with(h, |n| {
                let count = n.text.chars().count();
                let before = n.caret;
                n.caret = match code {
                    KEY_LEFT => n.caret.saturating_sub(1),
                    KEY_RIGHT => (n.caret + 1).min(count),
                    KEY_HOME | KEY_UP => 0,
                    _ => count,
                };
                n.anchor = if shift { Some(n.anchor.unwrap_or(before)) } else { None };
            });
        }
        _ => {}
    }
}

/// Deletes the selection, or a character before (`-1`) or after (`1`) the caret.
fn insert_or_delete(lua: &Lua, h: PanelHandle, direction: Option<i32>) {
    let changed = host(lua).panels.with(h, |n| {
        let mut chars: Vec<char> = n.text.chars().collect();
        let range = match (n.selection(), direction) {
            (Some(range), _) => range,
            (None, Some(-1)) if n.caret > 0 => (n.caret - 1, n.caret),
            (None, Some(1)) if n.caret < chars.len() => (n.caret, n.caret + 1),
            _ => return false,
        };
        chars.drain(range.0..range.1);
        n.caret = range.0;
        n.anchor = None;
        n.text = chars.into_iter().collect();
        true
    });
    if changed == Some(true) {
        call(lua, h, "OnTextChanged", ());
    }
}

// --- Lua -----------------------------------------------------------------------------------

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn def_many(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn nothing(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

fn many(a: &Args, values: impl IntoLuaMulti) -> mlua::Result<MultiValue> {
    values.into_lua_multi(a.lua)
}

/// The panel in argument 1, which must be there.
fn this(a: &Args) -> mlua::Result<PanelHandle> {
    let h = a.userdata::<PanelHandle>(1, "Panel")?;
    if host(a.lua).panels.is_valid(h) { Ok(h) } else { Err(args::error(a.lua, "Tried to use a NULL Panel!")) }
}

/// A panel argument that may be nil or gone: none then.
fn opt_panel(a: &Args, position: usize) -> Option<PanelHandle> {
    match a.get(position) {
        Some(Value::UserData(ud)) => ud.borrow::<PanelHandle>().ok().map(|h| *h).filter(|h| host(a.lua).panels.is_valid(*h)),
        _ => None,
    }
}

fn read<R>(a: &Args, f: impl FnOnce(&Node) -> R) -> mlua::Result<R> {
    let h = this(a)?;
    Ok(host(a.lua).panels.get(h, f).expect("checked above"))
}

fn write(a: &Args, f: impl FnOnce(&mut Node)) -> mlua::Result<Value> {
    let h = this(a)?;
    host(a.lua).panels.with(h, f);
    Ok(Value::Nil)
}

fn int(a: &Args, position: usize) -> mlua::Result<i32> {
    Ok(a.opt_number(position)?.unwrap_or(0.0) as i32)
}

fn integer(n: i32) -> mlua::Result<Value> {
    Ok(Value::Integer(n.into()))
}

fn string(a: &Args, text: &str) -> mlua::Result<Value> {
    Ok(Value::String(a.lua.create_string(text)?))
}

/// Makes a panel of one of the engine's classes, in `parent` (the world panel when none).
fn create(lua: &Lua, class: &str, parent: Option<PanelHandle>, name: Option<String>) -> Option<PanelHandle> {
    let &(class_name, kind) = CLASSES.iter().find(|(n, _)| n.eq_ignore_ascii_case(class))?;
    let panels = &host(lua).panels;
    let h = panels.create(Node::new(kind, class_name, name.unwrap_or_else(|| class_name.to_string())));
    set_parent(lua, h, parent.or(panels.world.get()));
    Some(h)
}

fn four(a: &Args, position: usize) -> mlua::Result<[i32; 4]> {
    Ok([int(a, position)?, int(a, position + 1)?, int(a, position + 2)?, int(a, position + 3)?])
}

/// The size of a label's text, inset included.
fn content_size(lua: &Lua, h: PanelHandle) -> (i32, i32) {
    let Some((class, text, font, inset, runs, w)) =
        host(lua).panels.get(h, |n| (n.class, n.text.clone(), n.font.clone(), n.inset, n.runs.clone(), n.w))
    else {
        return (0, 0);
    };
    let Some(font) = font_of(lua, font.as_deref()) else { return (0, 0) };
    match class {
        Class::Label | Class::Button | Class::TextEntry => {
            let (tw, th) = host(lua).screen.fonts.borrow_mut().text_size(font, &shown_text(lua, &text));
            (tw + inset.0, th + inset.1)
        }
        Class::RichText => {
            let tall = host(lua).screen.fonts.borrow().tall(font);
            (w, rich_lines(lua, font, &runs, w - 4).len() as i32 * tall)
        }
        _ => (0, 0),
    }
}

/// How far the children reach, with their margins and this panel's padding.
fn children_size(lua: &Lua, h: PanelHandle) -> (i32, i32) {
    let panels = &host(lua).panels;
    let padding = panels.get(h, |n| n.padding).unwrap_or_default();
    let (mut w, mut hh) = (0, 0);
    for child in panels.children(h) {
        if let Some((x, y, cw, ch, m, visible)) = panels.get(child, |n| (n.x, n.y, n.w, n.h, n.margin, n.visible && !n.removing))
            && visible
        {
            w = w.max(x + cw + m[2]);
            hh = hh.max(y + ch + m[3]);
        }
    }
    (w + padding[2], hh + padding[3])
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.set_named_registry_value(CACHE, lua.create_table()?)?;
    let panels = &host(lua).panels;
    let (sw, sh) = host(lua).screen.size.get();
    let world = panels.create(Node::new(Class::Panel, "Panel", "WorldPanel".to_string()));
    let hud = panels.create(Node::new(Class::Panel, "Panel", "HudPanel".to_string()));
    for root in [world, hud] {
        panels.with(root, |n| {
            n.w = sw as i32;
            n.h = sh as i32;
        });
    }
    panels.attach(hud, world);
    panels.with(hud, |n| n.mouse = false);
    panels.world.set(Some(world));
    panels.hud.set(Some(hud));

    let globals = lua.globals();
    for (name, value) in [("NODOCK", 0), ("FILL", 1), ("LEFT", 2), ("RIGHT", 3), ("TOP", 4), ("BOTTOM", 5)] {
        globals.raw_set(name, value)?;
    }
    def(lua, &globals, "GetHUDPanel", |a| push_opt(a.lua, host(a.lua).panels.hud.get()))?;

    let vgui = lua.create_table()?;
    def(lua, &vgui, "Create", |a| {
        let class = a.string(1)?;
        let parent = opt_panel(a, 2);
        let name = a.opt_string(3)?;
        push_opt(a.lua, create(a.lua, &class, parent, name))
    })?;
    def(lua, &vgui, "GetWorldPanel", |a| push_opt(a.lua, host(a.lua).panels.world.get()))?;
    def(lua, &vgui, "GetHoveredPanel", |a| push_opt(a.lua, host(a.lua).panels.hovered.get()))?;
    def(lua, &vgui, "GetKeyboardFocus", |a| push_opt(a.lua, host(a.lua).panels.focus.get()))?;
    def(lua, &vgui, "CursorVisible", |a| {
        let host = host(a.lua);
        Ok(Value::Boolean(host.panels.cursor_visible(host.client.screen_clicker.get())))
    })?;
    def(lua, &vgui, "IsHoveringWorld", |a| Ok(Value::Boolean(host(a.lua).panels.hovered.get().is_none_or(|h| is_world(a.lua, h)))))?;
    def(lua, &vgui, "FocusedHasParent", |a| {
        let panels = &host(a.lua).panels;
        let parent = opt_panel(a, 1);
        Ok(Value::Boolean(matches!((panels.focus.get(), parent), (Some(f), Some(p)) if panels.descends(f, p))))
    })?;
    def(lua, &vgui, "GetAll", |a| {
        let t = a.lua.create_table()?;
        for h in shown_panels(a.lua) {
            t.raw_push(push(a.lua, h)?)?;
        }
        Ok(Value::Table(t))
    })?;
    globals.raw_set("vgui", vgui)?;

    install_methods(lua, &meta::meta_table(lua, "Panel", type_id::PANEL)?)
}

fn install_methods(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // the tree
    def(lua, m, "IsValid", |a| Ok(Value::Boolean(opt_panel(a, 1).is_some())))?;
    def(lua, m, "Valid", |a| Ok(Value::Boolean(opt_panel(a, 1).is_some())))?;
    def(lua, m, "GetTable", |a| match a.get(1) {
        Some(Value::UserData(ud)) => Ok(table_of(ud)?.map_or(Value::Nil, Value::Table)),
        _ => Err(a.bad(1, "Panel")),
    })?;
    def(lua, m, "SetParent", |a| {
        let h = this(a)?;
        set_parent(a.lua, h, opt_panel(a, 2));
        Ok(Value::Nil)
    })?;
    def(lua, m, "GetParent", |a| {
        let parent = read(a, |n| n.parent)?;
        push_opt(a.lua, parent)
    })?;
    def(lua, m, "ParentToHUD", |a| {
        let h = this(a)?;
        let hud = host(a.lua).panels.hud.get();
        set_parent(a.lua, h, hud);
        Ok(Value::Nil)
    })?;
    def(lua, m, "GetChildren", |a| {
        let children = read(a, |n| n.children.clone())?;
        let t = a.lua.create_table()?;
        for c in children {
            t.raw_push(push(a.lua, c)?)?;
        }
        Ok(Value::Table(t))
    })?;
    def(lua, m, "GetChild", |a| {
        let n = int(a, 2)?;
        let child = read(a, |node| usize::try_from(n).ok().and_then(|i| node.children.get(i).copied()))?;
        push_opt(a.lua, child)
    })?;
    def(lua, m, "ChildCount", |a| integer(read(a, |n| n.children.len() as i32)?))?;
    def(lua, m, "HasChildren", |a| Ok(Value::Boolean(read(a, |n| !n.children.is_empty())?)))?;
    def(lua, m, "HasParent", |a| {
        let h = this(a)?;
        Ok(Value::Boolean(opt_panel(a, 2).is_some_and(|p| p != h && host(a.lua).panels.descends(h, p))))
    })?;
    def(lua, m, "Find", |a| {
        let h = this(a)?;
        let name = a.string(2)?;
        let panels = &host(a.lua).panels;
        let mut stack = panels.children(h);
        while let Some(c) = stack.pop() {
            if panels.get(c, |n| n.name == name).unwrap_or(false) {
                return push(a.lua, c);
            }
            stack.extend(panels.children(c));
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "Remove", |a| {
        if let Some(h) = opt_panel(a, 1) {
            remove(a.lua, h);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "IsMarkedForDeletion", |a| Ok(Value::Boolean(read(a, |n| n.removing)?)))?;
    def(lua, m, "Clear", |a| {
        let h = this(a)?;
        for c in host(a.lua).panels.children(h) {
            remove(a.lua, c);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "GetClassName", |a| {
        let name = read(a, |n| n.class_name)?;
        string(a, name)
    })?;
    def(lua, m, "GetName", |a| {
        let name = read(a, |n| n.name.clone())?;
        string(a, &name)
    })?;
    def(lua, m, "SetName", |a| {
        let name = a.opt_string(2)?.unwrap_or_default();
        write(a, |n| n.name = name)
    })?;
    def(lua, m, "MoveToFront", |a| {
        let h = this(a)?;
        move_to_front(a.lua, h);
        Ok(Value::Nil)
    })?;
    def(lua, m, "MoveToBack", |a| {
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        if let Some(parent) = panels.parent(h) {
            panels.with(parent, |n| {
                n.children.retain(|c| *c != h);
                n.children.insert(0, h);
            });
        }
        let mut popups = panels.popups.borrow_mut();
        if popups.contains(&h) {
            popups.retain(|p| *p != h);
            popups.insert(0, h);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "SetZPos", |a| {
        let z = int(a, 2)?;
        write(a, |n| n.z = z)
    })?;
    def(lua, m, "GetZPos", |a| integer(read(a, |n| n.z)?))?;
    def(lua, m, "MakePopup", |a| {
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        panels.with(h, |n| {
            n.popup = true;
            n.mouse = true;
            n.keyboard = true;
        });
        let world = panels.world.get();
        if panels.parent(h).is_none() {
            set_parent(a.lua, h, world);
        }
        {
            let mut popups = panels.popups.borrow_mut();
            popups.retain(|p| *p != h);
            popups.push(h);
        }
        set_focus(a.lua, Some(h));
        Ok(Value::Nil)
    })?;
    def(lua, m, "IsPopup", |a| Ok(Value::Boolean(read(a, |n| n.popup)?)))?;
    def(lua, m, "DoModal", |a| write(a, |n| n.modal = true))?;
    def(lua, m, "IsModal", |a| Ok(Value::Boolean(read(a, |n| n.modal)?)))?;
    def(lua, m, "SetDrawOnTop", |a| {
        let on = a.boolean(2);
        write(a, |n| n.on_top = on)
    })?;
    def(lua, m, "SetPopupStayAtBack", |a| {
        let on = a.boolean(2);
        write(a, |n| n.stay_at_back = on)
    })?;
    def(lua, m, "SetWorldClicker", |a| {
        let on = a.boolean(2);
        write(a, |n| n.world_clicker = on)
    })?;
    def(lua, m, "IsWorldClicker", |a| Ok(Value::Boolean(read(a, |n| n.world_clicker)?)))?;
    def(lua, m, "Prepare", |a| {
        let h = this(a)?;
        let hooks = has(a.lua, h, "OnChildAdded") || has(a.lua, h, "OnChildRemoved");
        host(a.lua).panels.with(h, |n| n.child_hooks = Some(hooks));
        Ok(Value::Nil)
    })?;
    for name in ["SetFocusTopLevel", "SetRenderInScreenshots", "SetAutoDelete", "SetTabPosition", "SetAllowNonAsciiCharacters"] {
        def(lua, m, name, nothing)?;
    }

    // where and how big
    def(lua, m, "SetPos", |a| {
        let h = this(a)?;
        let (x, y) = (int(a, 2)?, int(a, 3)?);
        set_pos(a.lua, h, Some(x), Some(y));
        Ok(Value::Nil)
    })?;
    def_many(lua, m, "GetPos", |a| {
        let pos = read(a, |n| (n.x, n.y))?;
        many(a, pos)
    })?;
    def(lua, m, "SetSize", |a| {
        let h = this(a)?;
        let (w, hh) = (int(a, 2)?, int(a, 3)?);
        set_size(a.lua, h, w, hh);
        Ok(Value::Nil)
    })?;
    def_many(lua, m, "GetSize", |a| {
        let size = read(a, |n| (n.w, n.h))?;
        many(a, size)
    })?;
    def(lua, m, "GetWide", |a| integer(read(a, |n| n.w)?))?;
    def(lua, m, "GetTall", |a| integer(read(a, |n| n.h)?))?;
    def_many(lua, m, "GetBounds", |a| {
        let bounds = read(a, |n| (n.x, n.y, n.w, n.h))?;
        many(a, bounds)
    })?;
    def(lua, m, "SetMinimumSize", |a| {
        let (w, hh) = (int(a, 2)?, int(a, 3)?);
        write(a, |n| {
            n.min_w = w;
            n.min_h = hh;
        })
    })?;
    def_many(lua, m, "LocalToScreen", |a| {
        let h = this(a)?;
        let (sx, sy) = host(a.lua).panels.screen_pos(h);
        many(a, (sx + int(a, 2)?, sy + int(a, 3)?))
    })?;
    def_many(lua, m, "ScreenToLocal", |a| {
        let h = this(a)?;
        let (sx, sy) = host(a.lua).panels.screen_pos(h);
        many(a, (int(a, 2)? - sx, int(a, 3)? - sy))
    })?;
    def_many(lua, m, "CursorPos", |a| {
        let h = this(a)?;
        let host = host(a.lua);
        let (sx, sy) = host.panels.screen_pos(h);
        let (cx, cy) = host.client.cursor.get();
        many(a, (cx - sx, cy - sy))
    })?;
    def_many(lua, m, "ChildrenSize", |a| {
        let h = this(a)?;
        many(a, children_size(a.lua, h))
    })?;
    def(lua, m, "SizeToChildren", |a| {
        let h = this(a)?;
        let (fit_w, fit_h) = (a.boolean(2), a.boolean(3));
        let (cw, ch) = children_size(a.lua, h);
        let (w, hh) = read(a, |n| (n.w, n.h))?;
        set_size(a.lua, h, if fit_w { cw } else { w }, if fit_h { ch } else { hh });
        Ok(Value::Nil)
    })?;
    def(lua, m, "SizeToContents", |a| {
        let h = this(a)?;
        if read(a, |n| n.has_text())? {
            let (w, hh) = content_size(a.lua, h);
            set_size(a.lua, h, w, hh);
        }
        Ok(Value::Nil)
    })?;
    def_many(lua, m, "GetContentSize", |a| {
        let h = this(a)?;
        many(a, content_size(a.lua, h))
    })?;
    def(lua, m, "Dock", |a| {
        let h = this(a)?;
        let dock = Dock::from_number(a.opt_number(2)?.unwrap_or(0.0) as i64);
        let panels = &host(a.lua).panels;
        let parent = panels.with(h, |n| {
            n.dock = dock;
            n.parent
        });
        if let Some(Some(parent)) = parent {
            panels.with(parent, |n| n.dirty = true);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "GetDock", |a| Ok(Value::Integer(read(a, |n| n.dock.number())?)))?;
    def(lua, m, "DockMargin", |a| {
        let margin = four(a, 2)?;
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        if let Some(Some(parent)) = panels.with(h, |n| {
            n.margin = margin;
            n.parent
        }) {
            panels.with(parent, |n| n.dirty = true);
        }
        Ok(Value::Nil)
    })?;
    def_many(lua, m, "GetDockMargin", |a| {
        let m = read(a, |n| n.margin)?;
        many(a, (m[0], m[1], m[2], m[3]))
    })?;
    def(lua, m, "DockPadding", |a| {
        let padding = four(a, 2)?;
        write(a, |n| {
            n.padding = padding;
            n.dirty = true;
        })
    })?;
    def_many(lua, m, "GetDockPadding", |a| {
        let p = read(a, |n| n.padding)?;
        many(a, (p[0], p[1], p[2], p[3]))
    })?;
    def(lua, m, "InvalidateLayout", |a| {
        let h = this(a)?;
        let now = a.boolean(2);
        host(a.lua).panels.with(h, |n| n.dirty = true);
        if now {
            perform_layout(a.lua, h);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "InvalidateChildren", |a| {
        let h = this(a)?;
        let (recursive, panels) = (a.boolean(2), &host(a.lua).panels);
        let mut stack = vec![h];
        while let Some(p) = stack.pop() {
            panels.with(p, |n| n.dirty = true);
            let children = panels.children(p);
            if recursive || p == h {
                stack.extend(children);
            }
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "NoClipping", |a| {
        let on = a.boolean(2);
        write(a, |n| n.no_clip = on)
    })?;

    // showing, input
    def(lua, m, "SetVisible", |a| {
        let visible = a.boolean(2);
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        let changed = panels.with(h, |n| {
            let changed = n.visible != visible;
            n.visible = visible;
            n.dirty |= changed && visible;
            (changed, n.dock != Dock::None, n.parent)
        });
        if let Some((true, true, Some(parent))) = changed {
            panels.with(parent, |n| n.dirty = true);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "IsVisible", |a| Ok(Value::Boolean(read(a, |n| n.visible)?)))?;
    def(lua, m, "SetAlpha", |a| {
        let alpha = a.opt_number(2)?.unwrap_or(255.0).clamp(0.0, 255.0) as u8;
        write(a, |n| n.alpha = alpha)
    })?;
    def(lua, m, "GetAlpha", |a| integer(read(a, |n| i32::from(n.alpha))?))?;
    def(lua, m, "SetEnabled", |a| {
        let on = a.boolean(2);
        write(a, |n| n.enabled = on)
    })?;
    def(lua, m, "IsEnabled", |a| Ok(Value::Boolean(read(a, |n| n.enabled)?)))?;
    def(lua, m, "SetMouseInputEnabled", |a| {
        let on = a.boolean(2);
        write(a, |n| n.mouse = on)
    })?;
    def(lua, m, "IsMouseInputEnabled", |a| Ok(Value::Boolean(read(a, |n| n.mouse)?)))?;
    for name in ["SetKeyboardInputEnabled", "SetKeyBoardInputEnabled"] {
        def(lua, m, name, |a| {
            let on = a.boolean(2);
            write(a, |n| n.keyboard = on)
        })?;
    }
    def(lua, m, "IsKeyboardInputEnabled", |a| Ok(Value::Boolean(read(a, |n| n.keyboard)?)))?;
    def(lua, m, "RequestFocus", |a| {
        let h = this(a)?;
        set_focus(a.lua, Some(h));
        Ok(Value::Nil)
    })?;
    def(lua, m, "KillFocus", |a| {
        let h = this(a)?;
        if host(a.lua).panels.focus.get() == Some(h) {
            set_focus(a.lua, None);
        }
        Ok(Value::Nil)
    })?;
    for name in ["FocusNext", "FocusPrevious"] {
        def(lua, m, name, |a| {
            this(a)?;
            set_focus(a.lua, None);
            Ok(Value::Nil)
        })?;
    }
    def(lua, m, "HasFocus", |a| {
        let h = this(a)?;
        Ok(Value::Boolean(host(a.lua).panels.focus.get() == Some(h)))
    })?;
    def(lua, m, "HasHierarchicalFocus", |a| {
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        Ok(Value::Boolean(panels.focus.get().is_some_and(|f| panels.descends(f, h))))
    })?;
    def(lua, m, "MouseCapture", |a| {
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        if a.boolean(2) {
            panels.capture.set(Some(h));
        } else if panels.capture.get() == Some(h) {
            panels.capture.set(None);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "SetCursor", |a| {
        let cursor = a.opt_string(2)?.unwrap_or_default();
        write(a, |n| n.cursor = cursor)
    })?;
    def(lua, m, "SetPaintBackgroundEnabled", |a| {
        let on = a.boolean(2);
        write(a, |n| n.paint_background = on)
    })?;
    def(lua, m, "SetPaintBorderEnabled", nothing)?;
    def(lua, m, "SetPaintedManually", |a| {
        let on = a.boolean(2);
        write(a, |n| n.manual = on)
    })?;
    def(lua, m, "PaintManual", |a| {
        let h = this(a)?;
        let panels = &host(a.lua).panels;
        let origin = panels.parent(h).map_or((0, 0), |p| panels.screen_pos(p));
        let was = panels.with(h, |n| std::mem::replace(&mut n.manual, false)).unwrap_or(false);
        paint_panel(a.lua, h, origin, screen_clip(a.lua), 1.0, 0);
        panels.with(h, |n| n.manual = was);
        leave(a.lua);
        Ok(Value::Nil)
    })?;
    def(lua, m, "PaintAt", |a| {
        let h = this(a)?;
        let (x, y) = (int(a, 2)?, int(a, 3)?);
        let panels = &host(a.lua).panels;
        let (px, py) = read(a, |n| (n.x, n.y))?;
        paint_panel(a.lua, h, (x - px, y - py), screen_clip(a.lua), 1.0, 0);
        leave(a.lua);
        let _ = panels;
        Ok(Value::Nil)
    })?;
    def(lua, m, "SetFGColor", |a| {
        let color = color_args(a, 2)?;
        write(a, |n| n.fg = color)
    })?;
    def(lua, m, "SetBGColor", |a| {
        let color = color_args(a, 2)?;
        write(a, |n| n.bg = color)
    })?;
    def(lua, m, "GetFGColor", |a| {
        let color = read(a, |n| n.fg)?;
        push_color(a.lua, color)
    })?;
    def(lua, m, "GetBGColor", |a| {
        let color = read(a, |n| n.bg)?;
        push_color(a.lua, color)
    })?;

    // drawing in a panel's Paint
    def(lua, m, "DrawFilledRect", |a| {
        let (w, hh) = read(a, |n| (n.w, n.h))?;
        let screen = &host(a.lua).screen;
        let color = screen.color(screen.draw_color());
        screen.canvas.borrow_mut().rect(0.0, 0.0, w as f32, hh as f32, color);
        Ok(Value::Nil)
    })?;
    def(lua, m, "DrawOutlinedRect", |a| {
        let (w, hh) = read(a, |n| (n.w, n.h))?;
        let screen = &host(a.lua).screen;
        let color = screen.color(screen.draw_color());
        screen.canvas.borrow_mut().outlined_rect(0.0, 0.0, w as f32, hh as f32, 1.0, color);
        Ok(Value::Nil)
    })?;
    def(lua, m, "DrawTexturedRect", |a| {
        let (w, hh) = read(a, |n| (n.w, n.h))?;
        host(a.lua).screen.textured_rect([0.0, 0.0, w as f32, hh as f32], [0.0, 0.0, 1.0, 1.0]);
        Ok(Value::Nil)
    })?;
    def(lua, m, "DrawTextEntryText", |a| {
        let h = this(a)?;
        let text = color_args(a, 2)?;
        let highlight = color_args(a, 3)?;
        let caret = color_args(a, 4)?;
        draw_entry(a.lua, h, text, highlight, caret);
        Ok(Value::Nil)
    })?;
    def(lua, m, "DrawSelections", nothing)?;

    // text
    def(lua, m, "SetText", |a| {
        let text = a.opt_string(2)?.unwrap_or_default();
        write(a, |n| {
            n.caret = n.caret.min(text.chars().count());
            n.anchor = None;
            if n.class == Class::RichText {
                n.runs = vec![Run { text: text.clone(), color: n.run_color }];
            }
            n.text = text;
        })
    })?;
    def(lua, m, "GetText", |a| {
        let text = read(a, |n| if n.class == Class::RichText { n.runs.iter().map(|r| r.text.as_str()).collect() } else { n.text.clone() })?;
        string(a, &text)
    })?;
    def(lua, m, "GetValue", |a| {
        let text = read(a, |n| n.has_text().then(|| n.text.clone()))?;
        match text {
            Some(text) => string(a, &text),
            None => Ok(Value::Nil),
        }
    })?;
    def(lua, m, "SetFontInternal", |a| {
        let font = a.opt_string(2)?;
        write(a, |n| n.font = font)
    })?;
    def(lua, m, "GetFont", |a| {
        let font = read(a, |n| n.font.clone())?;
        match font {
            Some(font) => string(a, &font),
            None => Ok(Value::Nil),
        }
    })?;
    def(lua, m, "SetContentAlignment", |a| {
        let align = int(a, 2)?;
        write(a, |n| n.align = align)
    })?;
    def(lua, m, "GetContentAlignment", |a| integer(read(a, |n| n.align)?))?;
    def(lua, m, "SetTextInset", |a| {
        let inset = (int(a, 2)?, int(a, 3)?);
        write(a, |n| n.inset = inset)
    })?;
    def_many(lua, m, "GetTextInset", |a| {
        let inset = read(a, |n| n.inset)?;
        many(a, inset)
    })?;
    def_many(lua, m, "GetTextSize", |a| {
        let h = this(a)?;
        let (text, font) = read(a, |n| (n.text.clone(), n.font.clone()))?;
        let size = match font_of(a.lua, font.as_deref()) {
            Some(font) => host(a.lua).screen.fonts.borrow_mut().text_size(font, &shown_text(a.lua, &text)),
            None => (0, 0),
        };
        let _ = h;
        many(a, size)
    })?;
    def(lua, m, "SetWrap", |a| {
        let on = a.boolean(2);
        write(a, |n| n.wrap = on)
    })?;
    def(lua, m, "SetExpensiveShadow", |a| {
        let distance = int(a, 2)?;
        let color = match a.get(3) {
            Some(_) => color_args(a, 3)?,
            None => [0, 0, 0, 190],
        };
        write(a, |n| n.shadow = (distance > 0).then_some((distance, color)))
    })?;
    for name in ["SetUnderlineFont", "SetDrawLanguageID", "SetDrawLanguageIDAtLeft", "SetTextSelectionColors", "SetTextHidden", "SetConVar"]
    {
        def(lua, m, name, nothing)?;
    }

    // text entries
    def(lua, m, "SetCaretPos", |a| {
        let at = a.opt_number(2)?.unwrap_or(0.0).max(0.0) as usize;
        write(a, |n| {
            n.caret = at.min(n.text.chars().count());
            n.anchor = None;
        })
    })?;
    def(lua, m, "GetCaretPos", |a| Ok(Value::Integer(read(a, |n| n.caret as i64)?)))?;
    def(lua, m, "GotoTextStart", |a| write(a, |n| n.caret = 0))?;
    def(lua, m, "GotoTextEnd", |a| write(a, |n| n.caret = n.text.chars().count()))?;
    for name in ["SelectAll", "SelectAllText"] {
        def(lua, m, name, |a| {
            write(a, |n| {
                n.anchor = Some(0);
                n.caret = n.text.chars().count();
            })
        })?;
    }
    def(lua, m, "SelectNone", |a| write(a, |n| n.anchor = None))?;
    def(lua, m, "SelectAllOnFocus", |a| write(a, |n| n.select_all_on_focus = true))?;
    def_many(lua, m, "GetSelectedTextRange", |a| {
        let range = read(a, |n| n.selection().unwrap_or((0, 0)))?;
        many(a, (range.0 as i64, range.1 as i64))
    })?;
    def(lua, m, "SetMultiline", |a| {
        let on = a.boolean(2);
        write(a, |n| n.multiline = on)
    })?;
    def(lua, m, "IsMultiline", |a| Ok(Value::Boolean(read(a, |n| n.multiline)?)))?;
    def(lua, m, "SetMaximumCharCount", |a| {
        let max = int(a, 2)?;
        write(a, |n| n.max_chars = max)
    })?;
    def(lua, m, "GetMaximumCharCount", |a| integer(read(a, |n| n.max_chars)?))?;
    def(lua, m, "CopySelected", |a| {
        let selected = read(a, |n| n.selection().map(|(f, t)| n.text.chars().skip(f).take(t - f).collect::<String>()))?;
        if let Some(text) = selected {
            *host(a.lua).panels.clipboard.borrow_mut() = text;
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "CutSelected", |a| {
        let h = this(a)?;
        let selected = read(a, |n| n.selection().map(|(f, t)| n.text.chars().skip(f).take(t - f).collect::<String>()))?;
        if let Some(text) = selected {
            *host(a.lua).panels.clipboard.borrow_mut() = text;
            insert_or_delete(a.lua, h, None);
        }
        Ok(Value::Nil)
    })?;
    def(lua, m, "Paste", |a| {
        let h = this(a)?;
        let text = host(a.lua).panels.clipboard.borrow().clone();
        insert(a.lua, h, &text);
        Ok(Value::Nil)
    })?;
    for name in ["Undo", "SaveUndoState", "SetVerticalScrollbarEnabled", "SetLineHeight", "ResetAllFades", "InsertFade"] {
        def(lua, m, name, nothing)?;
    }
    def(lua, m, "GetLineHeight", |a| {
        let font = read(a, |n| n.font.clone())?;
        integer(font_of(a.lua, font.as_deref()).map_or(0, |f| host(a.lua).screen.fonts.borrow().tall(f)))
    })?;
    def(lua, m, "GetNumLines", |a| {
        let h = this(a)?;
        let (class, text, font, runs, w) = read(a, |n| (n.class, n.text.clone(), n.font.clone(), n.runs.clone(), n.w))?;
        let Some(font) = font_of(a.lua, font.as_deref()) else { return integer(0) };
        let _ = h;
        integer(match class {
            Class::RichText => rich_lines(a.lua, font, &runs, w - 4).len() as i32,
            _ => text.split('\n').count() as i32,
        })
    })?;
    def_many(lua, m, "GetScrollStartIndexes", |a| many(a, (0, 0)))?;

    // rich text
    for name in ["AppendText", "AddText"] {
        def(lua, m, name, |a| {
            let text = a.opt_string(2)?.unwrap_or_default();
            write(a, |n| {
                let color = n.run_color;
                match n.runs.last_mut() {
                    Some(run) if run.color == color => run.text.push_str(&text),
                    _ => n.runs.push(Run { text, color }),
                }
            })
        })?;
    }
    def(lua, m, "AppendTextWithURLs", |a| {
        let text = a.opt_string(2)?.unwrap_or_default();
        write(a, |n| {
            let color = n.run_color;
            n.runs.push(Run { text, color });
        })
    })?;
    def(lua, m, "InsertColorChange", |a| {
        let color = color_args(a, 2)?;
        write(a, |n| n.run_color = color)
    })?;
    for name in ["InsertClickableTextStart", "InsertClickableTextEnd"] {
        def(lua, m, name, nothing)?;
    }
    def(lua, m, "SetToFullHeight", |a| {
        let h = this(a)?;
        let (w, hh) = content_size(a.lua, h);
        let _ = w;
        let width = read(a, |n| n.w)?;
        set_size(a.lua, h, width, hh);
        Ok(Value::Nil)
    })?;

    // pages: not shown, but they answer
    def(lua, m, "OpenURL", |a| {
        let url = a.opt_string(2)?.unwrap_or_default();
        write(a, |n| n.url = url)
    })?;
    def(lua, m, "SetURL", |a| {
        let url = a.opt_string(2)?.unwrap_or_default();
        write(a, |n| n.url = url)
    })?;
    def(lua, m, "IsLoading", |a| {
        this(a)?;
        Ok(Value::Boolean(false))
    })?;
    for name in [
        "SetHTML",
        "RunJavascript",
        "Exec",
        "NewObject",
        "NewObjectCallback",
        "StopLoading",
        "Refresh",
        "GoBack",
        "GoForward",
        "GoToHistoryOffset",
        "UpdateHTMLTexture",
    ] {
        def(lua, m, name, nothing)?;
    }
    def(lua, m, "GetHTMLMaterial", nothing)?;

    // spawn icons and avatars
    def(lua, m, "SetModel", |a| {
        let model = a.opt_string(2)?.unwrap_or_default().replace('\\', "/");
        let h = this(a)?;
        let icon = spawn_icon(a.lua, &model);
        host(a.lua).panels.with(h, |n| {
            n.model = model;
            n.icon = icon;
        });
        Ok(Value::Nil)
    })?;
    def(lua, m, "SetSpawnIcon", |a| {
        let path = a.opt_string(2)?.unwrap_or_default();
        let h = this(a)?;
        let icon = image_if_there(a.lua, &path);
        host(a.lua).panels.with(h, |n| n.icon = icon.or(n.icon));
        Ok(Value::Nil)
    })?;
    for name in ["RebuildSpawnIcon", "RebuildSpawnIconEx", "SetPlayer", "SetSteamID", "SetAchievement", "LoadTGAImage"] {
        def(lua, m, name, nothing)?;
    }
    def(lua, m, "LoadControlsFromString", nothing)?;
    def(lua, m, "LoadControlsFromFile", nothing)?;
    Ok(())
}

/// The image of a file under `materials/`, if the game has it.
fn image_if_there(lua: &Lua, path: &str) -> Option<u32> {
    let host = host(lua);
    host.vfs.exists(&format!("materials/{path}"), "GAME").then(|| host.screen.image_id(path, false))
}

/// The spawn icon the game has of a model: `spawnicons/models/....png`.
fn spawn_icon(lua: &Lua, model: &str) -> Option<u32> {
    let stem = model.to_lowercase();
    let stem = stem.strip_suffix(".mdl").unwrap_or(&stem);
    image_if_there(lua, &format!("spawnicons/{stem}.png"))
}
