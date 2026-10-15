//! The screen of a client and `surface`, the 2D drawing that the HUD, Derma and every
//! `HUDPaint` go through: rectangles, lines, textured rectangles, polygons and text, in
//! pixels from the top left corner, into the frame's [`Canvas`] - which the program draws over
//! the 3D view.
//!
//! Fonts are [`gmod_ui::Fonts`]: the font files of the game (`resource/*.ttf` - the HUD's
//! `HalfLife2`, Derma's `Marlett` - and `resource/fonts/`, where addons put theirs) and the
//! system's. Those of `surface.CreateFont` are found by name first, then those of the
//! engine's schemes (`resource/ClientScheme.res`, `SourceScheme.res`: `Default`,
//! `HudNumbers`, `TargetID`, `Trebuchet24` ...), each in the size its scheme gives for the
//! height of the screen.
//!
//! Images - materials (`gui/gradient`), PNG files (`icon16/add.png`) - are numbered as they
//! are first drawn or asked for (`surface.GetTextureID`, `Material`); the program loads them
//! by name ([`crate::Runtime::image`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gmod_material::png::Rgba;

use gmod_ui::{Canvas, Clip, FontId, FontSpec, Fonts, Texture};
use mlua::{Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::host;

/// What `surface` draws with: colours, the text's place, the font, the texture.
#[derive(Debug, Clone)]
struct Pen {
    draw: [u8; 4],
    text: [u8; 4],
    text_pos: (f32, f32),
    font: Option<FontId>,
    /// An image, and whether it adds light.
    texture: Option<(u32, bool)>,
    alpha: f32,
    /// `surface.DisableClipping`: panels do not keep drawing inside them.
    no_clip: bool,
}

impl Default for Pen {
    fn default() -> Pen {
        Pen { draw: [255; 4], text: [255; 4], text_pos: (0.0, 0.0), font: None, texture: None, alpha: 1.0, no_clip: false }
    }
}

/// An image to draw: a material by name, or an image file (`.png`, `.jpg`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub name: String,
    /// The material adds light (`$additive`).
    pub additive: bool,
}

/// A font of a scheme: its variants, each for screens of a range of heights.
#[derive(Debug, Clone)]
struct SchemeFont {
    variants: Vec<(Option<(u32, u32)>, FontSpec)>,
}

impl SchemeFont {
    /// The variant for a screen `height` pixels tall, as the engine's HUD asks for it: one
    /// made for that height as it is, else the one for any height grown from a screen 480
    /// pixels tall to this one.
    fn proportional(&self, height: u32) -> Option<FontSpec> {
        if let Some((_, spec)) = self.variants.iter().find(|(range, _)| range.is_some_and(|(lo, hi)| (lo..=hi).contains(&height))) {
            return Some(spec.clone());
        }
        let mut spec = self.variants.iter().find(|(range, _)| range.is_none()).or_else(|| self.variants.first())?.1.clone();
        let scale = |n: u32| if n == 0 { 0 } else { ((n as f32 * height as f32 / 480.0).round() as u32).max(1) };
        spec.size = scale(spec.size);
        spec.blur = scale(spec.blur);
        spec.scanlines = scale(spec.scanlines);
        Some(spec)
    }

    fn for_height(&self, height: u32) -> Option<&FontSpec> {
        self.variants
            .iter()
            .find(|(range, _)| range.is_some_and(|(lo, hi)| (lo..=hi).contains(&height)))
            .or_else(|| self.variants.iter().find(|(range, _)| range.is_none()))
            .or_else(|| self.variants.first())
            .map(|(_, spec)| spec)
    }
}

/// A client's screen: its size, the frame being drawn, fonts and images.
pub(crate) struct Screen {
    pub size: Cell<(u32, u32)>,
    pub canvas: RefCell<Canvas>,
    pub fonts: RefCell<Fonts>,
    /// Fonts of `surface.CreateFont`, by lower-case name.
    named: RefCell<HashMap<String, FontId>>,
    scheme: RefCell<HashMap<String, SchemeFont>>,
    pen: RefCell<Pen>,
    images: RefCell<Vec<Image>>,
    image_ids: RefCell<HashMap<String, u32>>,
    /// Models the program is to picture, spawn icons the game has none of: the image each
    /// becomes, and the model.
    pub model_icons: RefCell<Vec<(u32, String)>>,
    /// Image sizes measured, by number.
    image_sizes: RefCell<HashMap<u32, (u32, u32)>>,
    /// Pixels read for `GetColor`, by lower-case name; none for what is not there.
    pixels: RefCell<HashMap<String, Option<Rc<Rgba>>>>,
    /// Where panels keep drawing, and `render.SetScissorRect`'s rectangle.
    pub panel_clip: Cell<Option<Clip>>,
    pub scissor: Cell<Option<Clip>>,
}

impl Default for Screen {
    fn default() -> Screen {
        Screen {
            size: Cell::new((1280, 720)),
            canvas: RefCell::new(Canvas::new(1280, 720)),
            fonts: RefCell::new(Fonts::new()),
            named: RefCell::new(HashMap::new()),
            scheme: RefCell::new(HashMap::new()),
            pen: RefCell::new(Pen::default()),
            images: RefCell::new(Vec::new()),
            image_ids: RefCell::new(HashMap::new()),
            model_icons: RefCell::new(Vec::new()),
            image_sizes: RefCell::new(HashMap::new()),
            pixels: RefCell::new(HashMap::new()),
            panel_clip: Cell::new(None),
            scissor: Cell::new(None),
        }
    }
}

impl Screen {
    /// A frame begins: an empty canvas the size of the screen, the pen as new.
    pub fn begin(&self) {
        let (w, h) = self.size.get();
        self.canvas.borrow_mut().reset(w, h);
        *self.pen.borrow_mut() = Pen::default();
        self.panel_clip.set(None);
        self.scissor.set(None);
    }

    /// The clip drawing is kept in now.
    pub fn update_clip(&self) {
        let panel = if self.pen.borrow().no_clip { None } else { self.panel_clip.get() };
        let clip = match (panel, self.scissor.get()) {
            (Some(a), Some(b)) => Some(a.intersect(b)),
            (a, b) => a.or(b),
        };
        self.canvas.borrow_mut().clip = clip;
    }

    /// The font of a name: one `surface.CreateFont` made, else the schemes'.
    pub fn font(&self, name: &str) -> Option<FontId> {
        let key = name.to_lowercase();
        if let Some(&font) = self.named.borrow().get(&key) {
            return Some(font);
        }
        let spec = self.scheme.borrow().get(&key)?.for_height(self.size.get().1)?.clone();
        Some(self.fonts.borrow_mut().font(&spec))
    }

    /// A font of the schemes the way the engine's HUD asks for them: proportional, as big
    /// on any screen ([`SchemeFont::proportional`]).
    pub fn proportional_font(&self, name: &str) -> Option<FontId> {
        let spec = self.scheme.borrow().get(&name.to_lowercase())?.proportional(self.size.get().1)?;
        Some(self.fonts.borrow_mut().font(&spec))
    }

    pub fn create_font(&self, name: &str, spec: &FontSpec) {
        let font = self.fonts.borrow_mut().font(spec);
        self.named.borrow_mut().insert(name.to_lowercase(), font);
    }

    /// The number of an image, given when first asked for.
    pub fn image_id(&self, name: &str, additive: bool) -> u32 {
        let key = name.replace('\\', "/").to_lowercase();
        if let Some(&id) = self.image_ids.borrow().get(&key) {
            return id;
        }
        let mut images = self.images.borrow_mut();
        let id = images.len() as u32 + 1;
        images.push(Image { name: name.replace('\\', "/"), additive });
        self.image_ids.borrow_mut().insert(key, id);
        id
    }

    /// The image of a model as the program pictures it (`model-icon/<model>`), asked for the
    /// first time it is wanted ([`crate::Runtime::take_model_icons`]).
    pub fn model_icon(&self, model: &str) -> u32 {
        let model = model.replace('\\', "/").to_lowercase();
        let name = format!("model-icon/{model}");
        let new = !self.image_ids.borrow().contains_key(&name);
        let id = self.image_id(&name, false);
        if new {
            self.model_icons.borrow_mut().push((id, model));
        }
        id
    }

    pub fn image(&self, id: u32) -> Option<Image> {
        self.images.borrow().get(id.checked_sub(1)? as usize).cloned()
    }

    /// A colour as drawn now: its alpha by the alpha multiplier.
    pub fn color(&self, color: [u8; 4]) -> [u8; 4] {
        let alpha = self.pen.borrow().alpha;
        [color[0], color[1], color[2], (f32::from(color[3]) * alpha).round().clamp(0.0, 255.0) as u8]
    }

    /// `surface.SetAlphaMultiplier`: what a panel's alpha sets while it paints.
    pub fn set_alpha(&self, alpha: f32) {
        self.pen.borrow_mut().alpha = alpha;
    }

    /// `surface.SetDrawColor`'s colour.
    pub fn draw_color(&self) -> [u8; 4] {
        self.pen.borrow().draw
    }

    /// A rectangle of the current texture, or of colour when there is none.
    pub fn textured_rect(&self, rect: [f32; 4], uv: [f32; 4]) {
        let (texture, color) = {
            let pen = self.pen.borrow();
            (pen.texture, pen.draw)
        };
        let color = self.color(color);
        let (texture, additive) = match texture {
            Some((id, additive)) => (Texture::Image(id), additive),
            None => (Texture::White, false),
        };
        self.canvas.borrow_mut().image(texture, additive, rect, uv, color);
    }

    /// Draws text in a font at a place: how wide it was.
    pub fn text(&self, font: FontId, x: f32, y: f32, text: &str, color: [u8; 4]) -> f32 {
        let color = self.color(color);
        self.canvas.borrow_mut().text(&mut self.fonts.borrow_mut(), font, x, y, text, color)
    }
}

/// The size of an image: a PNG's from its header, a material's from its base texture.
fn measure(lua: &Lua, image: &Image) -> Option<(u32, u32)> {
    let vfs = host(lua).vfs.clone();
    if is_image_file(&image.name) {
        return image_size(&read_image_file(lua, &image.name)?);
    }
    let material = gmod_material::load_material(&vfs, &image.name).ok()?;
    let texture = material.params().iter().find(|(k, _)| k == "$basetexture").map(|(_, v)| v.clone())?;
    let vtf = gmod_material::load_texture(&vfs, &texture).ok()?;
    Some((vtf.width, vtf.height))
}

/// Width and height from a PNG's or JPEG's header.
pub(crate) fn image_size(data: &[u8]) -> Option<(u32, u32)> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 24 {
        let be = |at: usize| u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
        return Some((be(16), be(20)));
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        // JPEG: the first start-of-frame segment
        let mut at = 2;
        while at + 9 < data.len() {
            if data[at] != 0xFF {
                at += 1;
                continue;
            }
            let marker = data[at + 1];
            let len = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                let h = u32::from(u16::from_be_bytes([data[at + 5], data[at + 6]]));
                let w = u32::from(u16::from_be_bytes([data[at + 7], data[at + 8]]));
                return Some((w, h));
            }
            at += 2 + len;
        }
    }
    None
}

/// The pixels of an image file (`.png`), a texture (`is_texture`) or a material's base
/// texture, read once.
pub(crate) fn pixels(lua: &Lua, name: &str, is_texture: bool) -> Option<Rc<Rgba>> {
    let key = format!("{}:{}", u8::from(is_texture), name.replace('\\', "/").to_lowercase());
    if let Some(found) = host(lua).screen.pixels.borrow().get(&key) {
        return found.clone();
    }
    let image = decode(lua, name, is_texture).map(Rc::new);
    host(lua).screen.pixels.borrow_mut().insert(key, image.clone());
    image
}

/// Reads the pixels of an image file (`.png`), a texture (`is_texture`) or a material's base
/// texture: its first frame, at full size.
/// Whether a material's name is an image file used as one (`.png`, `.jpg`).
pub(crate) fn is_image_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

/// The bytes of an image file used as a material: under `materials/` (where `../` leaves it:
/// `../backgrounds/x.jpg`), else from the root.
pub(crate) fn read_image_file(lua: &Lua, name: &str) -> Option<Vec<u8>> {
    let vfs = host(lua).vfs.clone();
    [format!("materials/{name}"), name.to_string()]
        .iter()
        .filter_map(|p| gmod_fs::path::normalize(p))
        .find_map(|p| vfs.read(&p, "GAME").ok())
}

pub(crate) fn decode(lua: &Lua, name: &str, is_texture: bool) -> Option<Rgba> {
    let vfs = host(lua).vfs.clone();
    let lower = name.replace('\\', "/").to_lowercase();
    if is_image_file(&lower) {
        return read_image_file(lua, &lower).and_then(|data| gmod_material::jpeg::decode_image(&data).ok());
    }
    let texture = if is_texture {
        Some(lower)
    } else {
        gmod_material::load_material(&vfs, name)
            .ok()
            .and_then(|m| m.params().iter().find(|(k, _)| k == "$basetexture").map(|(_, v)| v.clone()))
    };
    let image = gmod_material::load_texture(&vfs, &texture?).ok()?.rgba8(0, 0, 0, 0)?;
    Some(Rgba { width: image.width, height: image.height, pixels: image.rgba })
}

/// The size of an image by number, measured once.
pub(crate) fn image_size_of(lua: &Lua, id: u32) -> Option<(u32, u32)> {
    let screen = &host(lua).screen;
    if let Some(&size) = screen.image_sizes.borrow().get(&id) {
        return Some(size);
    }
    let image = screen.image(id)?;
    let size = measure(lua, &image)?;
    screen.image_sizes.borrow_mut().insert(id, size);
    Some(size)
}

/// Loads the fonts of the game and the fonts of the schemes. The system's fonts are read
/// when a family is asked for that the game does not have.
pub(crate) fn load_fonts(lua: &Lua) {
    let host = host(lua);
    let vfs = host.vfs.clone();
    let mut fonts = host.screen.fonts.borrow_mut();
    fonts.use_system_fonts();
    for pattern in ["resource/*.ttf", "resource/*.otf", "resource/fonts/*.ttf", "resource/fonts/*.otf"] {
        let folder = pattern.rsplit_once('/').map_or("", |(f, _)| f);
        for file in vfs.find(pattern, "GAME").files {
            if let Ok(data) = vfs.read(&format!("{folder}/{file}"), "GAME") {
                fonts.add_game_font(data, &file);
            }
        }
    }
    drop(fonts);
    let mut scheme = host.screen.scheme.borrow_mut();
    for file in ["resource/SourceScheme.res", "resource/ClientScheme.res", "resource/ChatScheme.res"] {
        let Ok(data) = vfs.read(file, "GAME") else { continue };
        let (mut document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&data), gmod_kv::Options::default());
        document.root.apply_conditions(&gmod_kv::pc_symbol);
        let Some(fonts) = document.root.iter().find_map(|p| p.value.as_block()).and_then(|b| b.block("Fonts")) else { continue };
        for pair in fonts {
            let Some(block) = pair.value.as_block() else { continue };
            let variants: Vec<_> = block.iter().filter_map(|p| p.value.as_block()).map(scheme_variant).collect();
            if !variants.is_empty() {
                scheme.insert(pair.key.to_lowercase(), SchemeFont { variants });
            }
        }
    }
}

/// Text as the engine draws and measures it: a `#token` is the phrase of the game's language.
fn localized(lua: &Lua, text: String) -> String {
    if text.starts_with('#') { host(lua).client.phrase(&text) } else { text }
}

/// One variant of a scheme font: `name`, `tall`, `weight`, the effects, and `yres`.
fn scheme_variant(block: &gmod_kv::Block) -> (Option<(u32, u32)>, FontSpec) {
    let number = |key: &str| block.str(key).and_then(|v| v.trim().parse::<f64>().ok());
    let flag = |key: &str| number(key).is_some_and(|v| v != 0.0);
    let yres = block.str("yres").and_then(|v| {
        let mut parts = v.split_whitespace().filter_map(|p| p.parse::<u32>().ok());
        Some((parts.next()?, parts.next()?))
    });
    let spec = FontSpec {
        family: block.str("name").unwrap_or("Arial").to_string(),
        size: number("tall").unwrap_or(12.0).max(1.0) as u32,
        weight: number("weight").unwrap_or(400.0).max(0.0) as u32,
        italic: flag("italic"),
        antialias: flag("antialias"),
        additive: flag("additive"),
        shadow: flag("dropshadow"),
        outline: flag("outline"),
        blur: number("blur").unwrap_or(0.0).max(0.0) as u32,
        scanlines: number("scanlines").unwrap_or(0.0).max(0.0) as u32,
        underline: flag("underline"),
        strikeout: flag("strikeout"),
        symbol: flag("symbol"),
    };
    (yres, spec)
}

/// A colour from arguments: `(r, g, b, a)` or a `Color` table; alpha defaults to 255.
pub(crate) fn color_args(a: &Args, position: usize) -> mlua::Result<[u8; 4]> {
    if let Some(Value::Table(t)) = a.get(position) {
        return color_table(t);
    }
    let part = |p: usize, default: f64| -> mlua::Result<u8> { Ok(a.opt_number(p)?.unwrap_or(default).clamp(0.0, 255.0) as u8) };
    Ok([part(position, 255.0)?, part(position + 1, 255.0)?, part(position + 2, 255.0)?, part(position + 3, 255.0)?])
}

/// A `Color` table's `r`, `g`, `b`, `a`.
pub(crate) fn color_table(t: &Table) -> mlua::Result<[u8; 4]> {
    let part = |k: &str| -> mlua::Result<u8> {
        let v: Value = t.get(k)?;
        Ok(match v {
            Value::Integer(n) => n.clamp(0, 255) as u8,
            Value::Number(n) => n.clamp(0.0, 255.0) as u8,
            _ => 255,
        })
    };
    Ok([part("r")?, part("g")?, part("b")?, part("a")?])
}

/// A `Color` table, with `Color`'s metatable when the game's Lua has made it.
pub(crate) fn push_color(lua: &Lua, c: [u8; 4]) -> mlua::Result<Value> {
    if let Ok(Some(color)) = lua.globals().get::<Option<mlua::Function>>("Color") {
        return color.call((c[0], c[1], c[2], c[3]));
    }
    let t = lua.create_table()?;
    for (k, v) in ["r", "g", "b", "a"].iter().zip(c) {
        t.raw_set(*k, v)?;
    }
    Ok(Value::Table(t))
}

/// `surface.CreateFont`'s table.
fn font_spec(t: &Table) -> mlua::Result<FontSpec> {
    let defaults = FontSpec::default();
    let number = |k: &str| -> mlua::Result<Option<f64>> {
        Ok(match t.get::<Value>(k)? {
            Value::Integer(n) => Some(n as f64),
            Value::Number(n) => Some(n),
            _ => None,
        })
    };
    let flag = |k: &str, default: bool| -> mlua::Result<bool> {
        Ok(match t.get::<Value>(k)? {
            Value::Nil => default,
            Value::Boolean(b) => b,
            _ => true,
        })
    };
    Ok(FontSpec {
        family: t.get::<Option<String>>("font")?.unwrap_or(defaults.family),
        size: number("size")?.unwrap_or(13.0).clamp(4.0, 255.0) as u32,
        weight: number("weight")?.unwrap_or(500.0).max(0.0) as u32,
        italic: flag("italic", false)?,
        antialias: flag("antialias", true)?,
        additive: flag("additive", false)?,
        shadow: flag("shadow", false)?,
        outline: flag("outline", false)?,
        blur: number("blursize")?.unwrap_or(0.0).max(0.0) as u32,
        scanlines: number("scanlines")?.unwrap_or(0.0).max(0.0) as u32,
        underline: flag("underline", false)?,
        strikeout: flag("strikeout", false)?,
        symbol: flag("symbol", false)?,
    })
}

fn def(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<Value>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

/// A function that returns more than one value.
fn def_many(lua: &Lua, table: &Table, name: &'static str, f: fn(&Args) -> mlua::Result<MultiValue>) -> mlua::Result<()> {
    table.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
}

fn two(a: &Args, x: impl mlua::IntoLua, y: impl mlua::IntoLua) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([x.into_lua(a.lua)?, y.into_lua(a.lua)?]))
}

fn nothing(_: &Args) -> mlua::Result<Value> {
    Ok(Value::Nil)
}

fn number(n: f32) -> Value {
    Value::Number(f64::from(n))
}

/// The rectangle of `x, y, w, h` arguments from `position`.
fn rect_args(a: &Args, position: usize) -> mlua::Result<[f32; 4]> {
    Ok([a.number(position)? as f32, a.number(position + 1)? as f32, a.number(position + 2)? as f32, a.number(position + 3)? as f32])
}

/// Draws a rectangle of the current texture, or of colour when there is none.
fn textured(a: &Args, rect: [f32; 4], uv: [f32; 4]) -> mlua::Result<Value> {
    host(a.lua).screen.textured_rect(rect, uv);
    Ok(Value::Nil)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    def(lua, &globals, "ScrW", |a| Ok(Value::Integer(i64::from(host(a.lua).screen.size.get().0))))?;
    def(lua, &globals, "ScrH", |a| Ok(Value::Integer(i64::from(host(a.lua).screen.size.get().1))))?;

    let s = lua.create_table()?;
    def(lua, &s, "ScreenWidth", |a| Ok(Value::Integer(i64::from(host(a.lua).screen.size.get().0))))?;
    def(lua, &s, "ScreenHeight", |a| Ok(Value::Integer(i64::from(host(a.lua).screen.size.get().1))))?;
    def(lua, &s, "CreateFont", |a| {
        let name = a.string(1)?;
        let spec = font_spec(&a.table(2)?)?;
        host(a.lua).screen.create_font(&name, &spec);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "SetFont", |a| {
        let name = a.string(1)?;
        let screen = &host(a.lua).screen;
        let font = screen.font(&name).or_else(|| screen.font("Default"));
        screen.pen.borrow_mut().font = font;
        Ok(Value::Nil)
    })?;
    def_many(lua, &s, "GetTextSize", |a| {
        let text = localized(a.lua, a.string(1)?);
        let screen = &host(a.lua).screen;
        let font = screen.pen.borrow().font.or_else(|| screen.font("Default"));
        let (w, h) = match font {
            Some(font) => screen.fonts.borrow_mut().text_size(font, &text),
            None => (0, 0),
        };
        two(a, w, h)
    })?;
    def(lua, &s, "SetTextColor", |a| {
        let color = color_args(a, 1)?;
        host(a.lua).screen.pen.borrow_mut().text = color;
        Ok(Value::Nil)
    })?;
    def(lua, &s, "GetTextColor", |a| push_color(a.lua, host(a.lua).screen.pen.borrow().text))?;
    def(lua, &s, "SetTextPos", |a| {
        let (x, y) = (a.number(1)? as f32, a.number(2)? as f32);
        host(a.lua).screen.pen.borrow_mut().text_pos = (x, y);
        Ok(Value::Nil)
    })?;
    def_many(lua, &s, "GetTextPos", |a| {
        let (x, y) = host(a.lua).screen.pen.borrow().text_pos;
        two(a, x, y)
    })?;
    def(lua, &s, "DrawText", |a| {
        let text = localized(a.lua, a.string(1)?);
        let screen = &host(a.lua).screen;
        let (font, (x, y), color) = {
            let pen = screen.pen.borrow();
            (pen.font, pen.text_pos, pen.text)
        };
        let Some(font) = font.or_else(|| screen.font("Default")) else { return Ok(Value::Nil) };
        let width = screen.text(font, x, y, &text, color);
        screen.pen.borrow_mut().text_pos = (x + width, y);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "SetDrawColor", |a| {
        let color = color_args(a, 1)?;
        host(a.lua).screen.pen.borrow_mut().draw = color;
        Ok(Value::Nil)
    })?;
    def(lua, &s, "GetDrawColor", |a| push_color(a.lua, host(a.lua).screen.pen.borrow().draw))?;
    def(lua, &s, "SetAlphaMultiplier", |a| {
        let alpha = a.number(1)? as f32;
        host(a.lua).screen.pen.borrow_mut().alpha = alpha.clamp(0.0, 1.0);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "GetAlphaMultiplier", |a| Ok(number(host(a.lua).screen.pen.borrow().alpha)))?;
    def(lua, &s, "DisableClipping", |a| {
        let disable = a.boolean(1);
        let screen = &host(a.lua).screen;
        let before = std::mem::replace(&mut screen.pen.borrow_mut().no_clip, disable);
        screen.update_clip();
        Ok(Value::Boolean(before))
    })?;
    def(lua, &s, "DrawRect", |a| {
        let [x, y, w, h] = rect_args(a, 1)?;
        let screen = &host(a.lua).screen;
        let color = screen.color(screen.pen.borrow().draw);
        screen.canvas.borrow_mut().rect(x, y, w, h, color);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "DrawOutlinedRect", |a| {
        let [x, y, w, h] = rect_args(a, 1)?;
        let thickness = a.opt_number(5)?.unwrap_or(1.0) as f32;
        let screen = &host(a.lua).screen;
        let color = screen.color(screen.pen.borrow().draw);
        screen.canvas.borrow_mut().outlined_rect(x, y, w, h, thickness, color);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "DrawLine", |a| {
        let [x0, y0, x1, y1] = rect_args(a, 1)?;
        let screen = &host(a.lua).screen;
        let color = screen.color(screen.pen.borrow().draw);
        screen.canvas.borrow_mut().line([x0, y0], [x1, y1], color);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "DrawCircle", |a| {
        let (x, y, radius) = (a.number(1)? as f32, a.number(2)? as f32, a.number(3)? as f32);
        let screen = &host(a.lua).screen;
        let color = screen.color(if a.get(4).is_some() { color_args(a, 4)? } else { screen.pen.borrow().draw });
        let steps = ((radius * 2.0).clamp(8.0, 128.0)) as usize;
        let mut canvas = screen.canvas.borrow_mut();
        for i in 0..steps {
            let angle = |i: usize| (i as f32 / steps as f32) * std::f32::consts::TAU;
            let (s0, c0) = angle(i).sin_cos();
            let (s1, c1) = angle(i + 1).sin_cos();
            canvas.line([x + c0 * radius, y + s0 * radius], [x + c1 * radius, y + s1 * radius], color);
        }
        Ok(Value::Nil)
    })?;
    def(lua, &s, "GetTextureID", |a| {
        let name = a.string(1)?;
        Ok(Value::Integer(i64::from(host(a.lua).screen.image_id(&name, false))))
    })?;
    def(lua, &s, "GetTextureNameByID", |a| {
        let id = a.number(1)? as u32;
        Ok(match host(a.lua).screen.image(id) {
            Some(image) => Value::String(a.lua.create_string(image.name)?),
            None => Value::Nil,
        })
    })?;
    def_many(lua, &s, "GetTextureSize", |a| {
        let id = a.number(1)? as u32;
        let (w, h) = image_size_of(a.lua, id).unwrap_or((32, 32));
        two(a, w, h)
    })?;
    def(lua, &s, "SetTexture", |a| {
        let id = a.number(1)? as u32;
        let screen = &host(a.lua).screen;
        let additive = screen.image(id).is_some_and(|i| i.additive);
        screen.pen.borrow_mut().texture = (id > 0).then_some((id, additive));
        Ok(Value::Nil)
    })?;
    def(lua, &s, "SetMaterial", |a| {
        let texture = crate::material::image_of(a, 1)?;
        host(a.lua).screen.pen.borrow_mut().texture = texture;
        Ok(Value::Nil)
    })?;
    def(lua, &s, "DrawTexturedRect", |a| textured(a, rect_args(a, 1)?, [0.0, 0.0, 1.0, 1.0]))?;
    def(lua, &s, "DrawTexturedRectUV", |a| {
        let rect = rect_args(a, 1)?;
        let uv = rect_args(a, 5)?;
        textured(a, rect, uv)
    })?;
    def(lua, &s, "DrawTexturedRectRotated", |a| {
        let [x, y, w, h] = rect_args(a, 1)?;
        let degrees = a.number(5)? as f32;
        let screen = &host(a.lua).screen;
        let (texture, color) = {
            let pen = screen.pen.borrow();
            (pen.texture, pen.draw)
        };
        let color = screen.color(color);
        let (texture, additive) = texture.map_or((Texture::White, false), |(id, add)| (Texture::Image(id), add));
        screen.canvas.borrow_mut().rotated(texture, additive, [x, y], [w, h], degrees, color);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "DrawPoly", |a| {
        let points = a.table(1)?;
        let mut list = Vec::new();
        for point in points.sequence_values::<Table>() {
            let point = point?;
            let get = |k: &str| -> mlua::Result<f32> { Ok(point.get::<Option<f64>>(k)?.unwrap_or(0.0) as f32) };
            list.push([get("x")?, get("y")?, get("u")?, get("v")?]);
        }
        let screen = &host(a.lua).screen;
        let (texture, color) = {
            let pen = screen.pen.borrow();
            (pen.texture, pen.draw)
        };
        let color = screen.color(color);
        let (texture, additive) = texture.map_or((Texture::White, false), |(id, add)| (Texture::Image(id), add));
        screen.canvas.borrow_mut().polygon(texture, additive, &list, color);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "PlaySound", |a| {
        crate::sound::play_ui(a.lua, &a.string(1)?);
        Ok(Value::Nil)
    })?;
    def(lua, &s, "GetHUDTexture", |a| {
        let name = a.string(1)?;
        crate::material::push_named(a.lua, &format!("hud/{name}"))
    })?;
    def(lua, &s, "GetPanelPaintState", |a| {
        let screen = &host(a.lua).screen;
        let t = a.lua.create_table()?;
        let clip = screen.panel_clip.get();
        t.raw_set("translate_x", 0)?;
        t.raw_set("translate_y", 0)?;
        t.raw_set("scissor_enabled", clip.is_some())?;
        let c = clip.unwrap_or(Clip::new(0, 0, 0, 0));
        t.raw_set("scissor_left", c.left)?;
        t.raw_set("scissor_top", c.top)?;
        t.raw_set("scissor_right", c.right)?;
        t.raw_set("scissor_bottom", c.bottom)?;
        Ok(Value::Table(t))
    })?;
    def(lua, &s, "SetTextPosition", nothing)?;
    // whether drawing is cut, and to what: the scissor and the clip of the panel that paints
    // (`DModelPanel` cuts its model to it), else the whole screen
    def_many(lua, &s, "GetScissorRect", |a| {
        let screen = &host(a.lua).screen;
        let (w, h) = screen.size.get();
        let cut = screen.canvas.borrow().clip;
        let clip = cut.unwrap_or(Clip::new(0, 0, w as i32, h as i32));
        let rect = [clip.left, clip.top, clip.right, clip.bottom].map(|v| Value::Integer(v.into()));
        Ok(MultiValue::from_iter(std::iter::once(Value::Boolean(cut.is_some())).chain(rect)))
    })?;
    globals.raw_set("surface", s)?;
    Ok(())
}
