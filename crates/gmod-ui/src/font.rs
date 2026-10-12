//! Fonts as VGUI makes them. `surface.CreateFont` names a family, a height in pixels (`size`,
//! the engine's "tall": the whole cell, ascent and descent, as Windows measures it), a weight
//! and effects: a shadow, an outline, a blur (the glow of the HUD's numbers), scan lines,
//! additive drawing. Text is laid out by whole pixels, character by character, as the engine
//! does, and drawn from glyphs baked into pages of an atlas - effects and all, white on
//! transparent with black shadows and outlines, so that the colour of the text tints them.
//!
//! Font files come from the game (`resource/*.ttf` - the HUD's `HALFLIFE2.ttf`, Derma's
//! `marlett.ttf` - and `resource/fonts/`, where addons put theirs; the program hands them in
//! with [`Fonts::add_font`]) and from the system's font folders, read when a family is asked
//! for that the game does not have. A family is found by any of its names - the family, the
//! typographic family or the full name ("Roboto Bold") - and the face nearest to the weight
//! asked for; a family that is nowhere is replaced by one of [`FALLBACKS`], and characters a
//! font does not have come from them too. Bold asked of a family without a bold face is made
//! bolder by hand, as Windows does.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Families that take the place of one that is not there, in this order: what Windows has,
/// then what Linux and macOS usually have.
pub const FALLBACKS: &[&str] =
    &["tahoma", "verdana", "arial", "segoe ui", "dejavu sans", "liberation sans", "noto sans", "roboto", "helvetica", "freesans", "ubuntu"];

/// Side of an atlas page in pixels.
pub const PAGE_SIZE: u32 = 1024;

/// What `surface.CreateFont` takes, with GMod's defaults.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontSpec {
    pub family: String,
    /// Height of the cell in pixels.
    pub size: u32,
    /// 100 (thin) to 900 (black); 400 is regular, 700 bold.
    pub weight: u32,
    pub italic: bool,
    pub antialias: bool,
    /// Drawn adding light, not covering.
    pub additive: bool,
    /// A black copy one pixel down and right.
    pub shadow: bool,
    /// A black edge one pixel wide.
    pub outline: bool,
    /// Pixels of blur.
    pub blur: u32,
    /// Every this many rows one is left bright, the others darkened.
    pub scanlines: u32,
    pub underline: bool,
    pub strikeout: bool,
    /// The characters are a symbol font's (Marlett's arrows and boxes).
    pub symbol: bool,
}

impl Default for FontSpec {
    fn default() -> FontSpec {
        FontSpec {
            family: "Arial".to_string(),
            size: 13,
            weight: 500,
            italic: false,
            antialias: true,
            additive: false,
            shadow: false,
            outline: false,
            blur: 0,
            scanlines: 0,
            underline: false,
            strikeout: false,
            symbol: false,
        }
    }
}

/// A font made from a [`FontSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontId(pub u32);

/// A character as drawn: where its bitmap is in the atlas and how it sits on the line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub page: u32,
    /// Left, top, right and bottom of the bitmap in the page, 0 to 1.
    pub uv: [f32; 4],
    /// From the pen to the bitmap's left edge, and from the top of the line to its top edge.
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// How far the pen moves on.
    pub advance: i32,
}

/// A page of the atlas: RGBA, rows top to bottom.
pub struct Page {
    pub pixels: Vec<u8>,
    /// Changed since [`Fonts::take_changed`] last said so.
    pub changed: bool,
    shelf_y: u32,
    shelf_height: u32,
    shelf_x: u32,
}

impl Page {
    fn new() -> Page {
        Page { pixels: vec![0; (PAGE_SIZE * PAGE_SIZE * 4) as usize], changed: true, shelf_y: 0, shelf_height: 0, shelf_x: 0 }
    }

    /// A place for a bitmap of this size, one pixel apart from the others.
    fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (w, h) = (width + 1, height + 1);
        if w > PAGE_SIZE || h > PAGE_SIZE {
            return None;
        }
        if self.shelf_x + w > PAGE_SIZE {
            self.shelf_y += self.shelf_height;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + h > PAGE_SIZE {
            return None;
        }
        let at = (self.shelf_x, self.shelf_y);
        self.shelf_x += w;
        self.shelf_height = self.shelf_height.max(h);
        Some(at)
    }
}

/// Where a face's bytes are.
#[derive(Clone)]
enum Source {
    Memory(Arc<[u8]>),
    File(PathBuf),
}

/// A face of a font file, as its tables name it.
struct Face {
    source: Source,
    index: u32,
    /// Family, typographic family and full name, lower case.
    names: Vec<String>,
    full_name: String,
    weight: u16,
    italic: bool,
    /// Read when first used; none when it could not be.
    loaded: Option<Option<Arc<Loaded>>>,
}

/// A face ready to draw.
struct Loaded {
    data: Arc<[u8]>,
    index: u32,
    raster: fontdue::Font,
    units_per_em: f32,
    /// The cell as Windows measures it: above and below the base line, in font units.
    ascent: f32,
    descent: f32,
}

impl Loaded {
    fn read(source: &Source, index: u32) -> Option<Loaded> {
        let data: Arc<[u8]> = match source {
            Source::Memory(data) => data.clone(),
            Source::File(path) => std::fs::read(path).ok()?.into(),
        };
        let face = ttf_parser::Face::parse(&data, index).ok()?;
        let units_per_em = f32::from(face.units_per_em().max(1));
        let (mut ascent, mut descent) = match face.tables().os2 {
            Some(os2) => (f32::from(os2.windows_ascender()), f32::from(os2.windows_descender()).abs()),
            None => (0.0, 0.0),
        };
        if ascent + descent <= 0.0 {
            ascent = f32::from(face.ascender());
            descent = f32::from(face.descender()).abs();
        }
        if ascent + descent <= 0.0 {
            ascent = units_per_em * 0.8;
            descent = units_per_em * 0.2;
        }
        let settings = fontdue::FontSettings { collection_index: index, scale: 40.0, load_substitutions: false };
        let raster = fontdue::Font::from_bytes(&data[..], settings).ok()?;
        Some(Loaded { data: data.clone(), index, raster, units_per_em, ascent, descent })
    }

    /// The glyph of a character, if the face has one; a symbol font's characters are also
    /// looked for where symbol fonts keep them (U+F000 on).
    fn glyph_index(&self, ch: char) -> Option<u16> {
        let face = ttf_parser::Face::parse(&self.data, self.index).ok()?;
        let cmap = face.tables().cmap?;
        let code = u32::from(ch);
        for subtable in cmap.subtables {
            if subtable.is_unicode() {
                if let Some(id) = subtable.glyph_index(code) {
                    return Some(id.0);
                }
            } else if subtable.platform_id == ttf_parser::PlatformId::Windows && subtable.encoding_id == 0 {
                let found = subtable.glyph_index(code).or_else(|| subtable.glyph_index(0xF000 | (code & 0xFF)).filter(|_| code < 0x100));
                if let Some(id) = found {
                    return Some(id.0);
                }
            }
        }
        None
    }

    /// Pixels to the em for a cell `tall` pixels high.
    fn pixels_per_em(&self, tall: f32) -> f32 {
        tall * self.units_per_em / (self.ascent + self.descent)
    }
}

/// A font made: its face and measures.
struct Instance {
    spec: FontSpec,
    face: Option<usize>,
    /// The cell as asked for, and as tall as the effects make it.
    size: i32,
    tall: i32,
    ascent: i32,
    /// Bold made by hand: the face is lighter than asked.
    embolden: bool,
}

/// The fonts of a screen: the files known, the fonts made and their glyphs in the atlas.
#[derive(Default)]
pub struct Fonts {
    faces: Vec<Face>,
    /// Folders whose fonts are read when a family is not found among the faces known.
    system_folders: Vec<PathBuf>,
    system_read: bool,
    by_family: HashMap<String, Vec<usize>>,
    instances: Vec<Instance>,
    by_spec: HashMap<FontSpec, FontId>,
    glyphs: HashMap<(FontId, char), Glyph>,
    pages: Vec<Page>,
}

impl Fonts {
    /// No fonts yet: add the game's with [`add_font`](Self::add_font) and the system's with
    /// [`use_system_fonts`](Self::use_system_fonts).
    pub fn new() -> Fonts {
        Fonts::default()
    }

    /// Adds the faces of a font file (`.ttf`, `.otf`, or a `.ttc` collection): how many.
    pub fn add_font(&mut self, data: Vec<u8>) -> usize {
        let data: Arc<[u8]> = data.into();
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        let mut added = 0;
        for index in 0..count {
            let Ok(face) = ttf_parser::Face::parse(&data, index) else { continue };
            let names = face_names(face.names());
            let style = (face.weight().to_number(), face.is_italic());
            self.push_face(Source::Memory(data.clone()), index, names, style);
            added += 1;
        }
        added
    }

    /// Looks in the system's font folders when a family is not among the fonts added.
    pub fn use_system_fonts(&mut self) {
        self.system_folders = system_font_folders();
    }

    /// Looks in these folders (and theirs) when a family is not among the fonts added.
    pub fn use_font_folders(&mut self, folders: Vec<PathBuf>) {
        self.system_folders = folders;
        self.system_read = false;
    }

    fn push_face(&mut self, source: Source, index: u32, names: (Vec<String>, String), style: (u16, bool)) {
        let (names, full_name) = names;
        let at = self.faces.len();
        for name in &names {
            self.by_family.entry(name.clone()).or_default().push(at);
        }
        self.faces.push(Face { source, index, names, full_name, weight: style.0, italic: style.1, loaded: None });
    }

    fn read_system_fonts(&mut self) {
        if self.system_read {
            return;
        }
        self.system_read = true;
        let mut files = Vec::new();
        for folder in self.system_folders.clone() {
            font_files(&folder, &mut files, 0);
        }
        for path in files {
            for (index, names, style) in describe_file(&path) {
                self.push_face(Source::File(path.clone()), index, names, style);
            }
        }
    }

    /// The face of a family nearest to a weight, if the family is there.
    fn find(&mut self, family: &str, weight: u32, italic: bool) -> Option<usize> {
        let key = family.trim().to_lowercase();
        if !self.by_family.contains_key(&key) {
            self.read_system_fonts();
        }
        let candidates = self.by_family.get(&key)?;
        // a full name ("Roboto Bold") is that face
        if let Some(&exact) = candidates.iter().find(|&&i| self.faces[i].full_name == key && self.faces[i].names[0] != key) {
            return Some(exact);
        }
        candidates.iter().copied().min_by_key(|&i| {
            let face = &self.faces[i];
            (u32::from(face.weight)).abs_diff(weight) + if face.italic == italic { 0 } else { 1000 }
        })
    }

    fn loaded(&mut self, face: usize) -> Option<Arc<Loaded>> {
        let entry = &mut self.faces[face];
        if entry.loaded.is_none() {
            entry.loaded = Some(Loaded::read(&entry.source, entry.index).map(Arc::new));
        }
        entry.loaded.clone().flatten()
    }

    /// The face for a spec: its family's, or a fallback's.
    fn resolve(&mut self, spec: &FontSpec) -> Option<usize> {
        let weight = spec.weight.clamp(1, 1000);
        if let Some(face) = self.find(&spec.family, weight, spec.italic).filter(|&f| self.loaded(f).is_some()) {
            return Some(face);
        }
        for family in FALLBACKS {
            if let Some(face) = self.find(family, weight, spec.italic).filter(|&f| self.loaded(f).is_some()) {
                return Some(face);
            }
        }
        (0..self.faces.len()).find(|&f| self.loaded(f).is_some())
    }

    /// The font for a spec, made once.
    pub fn font(&mut self, spec: &FontSpec) -> FontId {
        let key = FontSpec { family: spec.family.trim().to_lowercase(), ..spec.clone() };
        if let Some(&id) = self.by_spec.get(&key) {
            return id;
        }
        let face = self.resolve(spec);
        let size = spec.size.clamp(1, 1024) as i32;
        let (ascent, embolden) = match face {
            Some(f) => {
                let loaded = self.loaded(f).expect("resolved faces load");
                let scale = loaded.pixels_per_em(size as f32) / loaded.units_per_em;
                ((loaded.ascent * scale).round() as i32, spec.weight >= 600 && self.faces[f].weight < 600)
            }
            None => ((size as f32 * 0.8).round() as i32, false),
        };
        let tall = size + i32::from(spec.shadow) + 2 * i32::from(spec.outline);
        let id = FontId(self.instances.len() as u32);
        self.instances.push(Instance { spec: spec.clone(), face, size, tall, ascent, embolden });
        self.by_spec.insert(key, id);
        id
    }

    /// The spec a font was made from.
    pub fn spec(&self, font: FontId) -> &FontSpec {
        &self.instances[font.0 as usize].spec
    }

    /// The family of the face a font is drawn with, as its file names it; none when there are
    /// no fonts at all.
    pub fn face_name(&self, font: FontId) -> Option<&str> {
        let face = self.instances[font.0 as usize].face?;
        self.faces[face].names.first().map(String::as_str)
    }

    /// How tall a line of the font is, effects included.
    pub fn tall(&self, font: FontId) -> i32 {
        self.instances[font.0 as usize].tall
    }

    /// Width and height of text: the widest line, and a line's height for each line.
    pub fn text_size(&mut self, font: FontId, text: &str) -> (i32, i32) {
        let tall = self.tall(font);
        let (mut widest, mut x, mut lines) = (0, 0, 1);
        for ch in text.chars() {
            if ch == '\n' {
                widest = widest.max(x);
                x = 0;
                lines += 1;
            } else {
                x += self.glyph(font, ch).advance;
            }
        }
        (widest.max(x), tall * lines)
    }

    /// A character of a font, baked into the atlas the first time it is asked for.
    pub fn glyph(&mut self, font: FontId, ch: char) -> Glyph {
        if let Some(glyph) = self.glyphs.get(&(font, ch)) {
            return *glyph;
        }
        let glyph = self.bake(font, ch);
        self.glyphs.insert((font, ch), glyph);
        glyph
    }

    /// The face that draws a character: the font's, else the first fallback that has it.
    fn face_for(&mut self, font: FontId, ch: char) -> Option<(Arc<Loaded>, u16)> {
        let instance = &self.instances[font.0 as usize];
        let (face, weight, italic) = (instance.face, instance.spec.weight, instance.spec.italic);
        let face = face?;
        let loaded = self.loaded(face)?;
        if let Some(index) = loaded.glyph_index(ch) {
            return Some((loaded, index));
        }
        if !ch.is_control() && !ch.is_whitespace() {
            for family in FALLBACKS {
                let Some(other) = self.find(family, weight, italic) else { continue };
                let Some(fallback) = self.loaded(other) else { continue };
                if let Some(index) = fallback.glyph_index(ch) {
                    return Some((fallback, index));
                }
            }
        }
        Some((loaded, 0))
    }

    fn bake(&mut self, font: FontId, ch: char) -> Glyph {
        let empty = |advance| Glyph { page: 0, uv: [0.0; 4], left: 0, top: 0, width: 0, height: 0, advance };
        if ch.is_control() {
            return empty(0);
        }
        let (size, ascent, embolden, spec) = {
            let instance = &self.instances[font.0 as usize];
            (instance.size, instance.ascent, instance.embolden, instance.spec.clone())
        };
        let Some((loaded, index)) = self.face_for(font, ch) else { return empty(size / 2) };
        let px = loaded.pixels_per_em(size as f32);
        let (metrics, coverage) = loaded.raster.rasterize_indexed(index, px);
        let advance = metrics.advance_width.round() as i32 + i32::from(embolden);
        if metrics.width == 0 || metrics.height == 0 {
            return empty(advance);
        }
        let mut bitmap = Bitmap { width: metrics.width, height: metrics.height, alpha: coverage };
        if !spec.antialias {
            bitmap.alpha.iter_mut().for_each(|a| *a = if *a >= 128 { 255 } else { 0 });
        }
        if embolden {
            bitmap = bitmap.emboldened();
        }
        let blur = spec.blur.min(32) as usize;
        let outline = usize::from(spec.outline);
        let shadow = usize::from(spec.shadow);
        let pad = blur + outline;
        let rgba = bitmap.padded(pad, shadow).effects(blur, outline, shadow, spec.scanlines);
        let (width, height) = (rgba.width as u32, rgba.height as u32);
        let left = metrics.xmin - pad as i32;
        let top = ascent - (metrics.ymin + metrics.height as i32) - pad as i32;
        let Some((page, x, y)) = self.place(width, height) else { return empty(advance) };
        let target = &mut self.pages[page as usize];
        for row in 0..height as usize {
            let from = row * width as usize * 4;
            let to = ((y as usize + row) * PAGE_SIZE as usize + x as usize) * 4;
            target.pixels[to..to + width as usize * 4].copy_from_slice(&rgba.pixels[from..from + width as usize * 4]);
        }
        target.changed = true;
        let side = PAGE_SIZE as f32;
        Glyph {
            page,
            uv: [x as f32 / side, y as f32 / side, (x + width) as f32 / side, (y + height) as f32 / side],
            left,
            top,
            width,
            height,
            advance,
        }
    }

    fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32, u32)> {
        if let Some(last) = self.pages.last_mut()
            && let Some((x, y)) = last.place(width, height)
        {
            return Some((self.pages.len() as u32 - 1, x, y));
        }
        let mut page = Page::new();
        let (x, y) = page.place(width, height)?;
        self.pages.push(page);
        Some((self.pages.len() as u32 - 1, x, y))
    }

    /// The atlas.
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// The pages changed since the last call, by number: what to send to the GPU again.
    pub fn take_changed(&mut self) -> Vec<u32> {
        let mut changed = Vec::new();
        for (i, page) in self.pages.iter_mut().enumerate() {
            if page.changed {
                page.changed = false;
                changed.push(i as u32);
            }
        }
        changed
    }
}

/// A glyph's coverage, 0 to 255.
struct Bitmap {
    width: usize,
    height: usize,
    alpha: Vec<u8>,
}

impl Bitmap {
    fn at(&self, x: isize, y: isize) -> u8 {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            0
        } else {
            self.alpha[y as usize * self.width + x as usize]
        }
    }

    /// One pixel wider, each pixel as strong as itself or the one to its left.
    fn emboldened(&self) -> Bitmap {
        let width = self.width + 1;
        let mut alpha = vec![0; width * self.height];
        for y in 0..self.height {
            for x in 0..width {
                alpha[y * width + x] = self.at(x as isize, y as isize).max(self.at(x as isize - 1, y as isize));
            }
        }
        Bitmap { width, height: self.height, alpha }
    }

    /// Room for effects: `pad` all round, `extra` more right and below.
    fn padded(&self, pad: usize, extra: usize) -> Bitmap {
        let width = self.width + 2 * pad + extra;
        let height = self.height + 2 * pad + extra;
        let mut alpha = vec![0; width * height];
        for y in 0..self.height {
            let to = (y + pad) * width + pad;
            alpha[to..to + self.width].copy_from_slice(&self.alpha[y * self.width..(y + 1) * self.width]);
        }
        Bitmap { width, height, alpha }
    }

    /// Blurred with a Gaussian `radius` pixels wide.
    fn blurred(&self, radius: usize) -> Bitmap {
        let sigma = (radius as f32 / 2.0).max(0.5);
        let kernel: Vec<f32> = (-(radius as isize)..=radius as isize).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = kernel.iter().sum();
        let pass = |source: &Bitmap, horizontal: bool| {
            let mut alpha = vec![0; source.alpha.len()];
            for y in 0..source.height {
                for x in 0..source.width {
                    let mut total = 0.0;
                    for (k, weight) in kernel.iter().enumerate() {
                        let d = k as isize - radius as isize;
                        let value = if horizontal { source.at(x as isize + d, y as isize) } else { source.at(x as isize, y as isize + d) };
                        total += f32::from(value) * weight;
                    }
                    alpha[y * source.width + x] = (total / sum).round().min(255.0) as u8;
                }
            }
            Bitmap { width: source.width, height: source.height, alpha }
        };
        pass(&pass(self, true), false)
    }

    /// The glyph in white over its outline and shadow in black, with scan lines: RGBA.
    fn effects(self, blur: usize, outline: usize, shadow: usize, scanlines: u32) -> Rgba {
        let glyph = if blur > 0 { self.blurred(blur) } else { self };
        let (width, height) = (glyph.width, glyph.height);
        // white over black: alpha and how bright, 0 to 1
        let mut alpha: Vec<f32> = glyph.alpha.iter().map(|&a| f32::from(a) / 255.0).collect();
        let mut bright = vec![1.0f32; alpha.len()];
        if outline > 0 {
            for y in 0..height {
                for x in 0..width {
                    let mut edge = 0u8;
                    for dy in -(outline as isize)..=outline as isize {
                        for dx in -(outline as isize)..=outline as isize {
                            edge = edge.max(glyph.at(x as isize + dx, y as isize + dy));
                        }
                    }
                    let i = y * width + x;
                    let (a, o) = (alpha[i], f32::from(edge) / 255.0);
                    let out = a + o * (1.0 - a);
                    bright[i] = if out > 0.0 { a / out } else { 1.0 };
                    alpha[i] = out;
                }
            }
        }
        if shadow > 0 {
            let before = alpha.clone();
            for y in 0..height {
                for x in 0..width {
                    let i = y * width + x;
                    let s = if x >= shadow && y >= shadow { before[(y - shadow) * width + x - shadow] } else { 0.0 };
                    let a = alpha[i];
                    let out = a + s * (1.0 - a);
                    bright[i] = if out > 0.0 { bright[i] * a / out } else { 1.0 };
                    alpha[i] = out;
                }
            }
        }
        if scanlines > 1 {
            for y in (0..height).filter(|y| y % scanlines as usize != 0) {
                bright[y * width..(y + 1) * width].iter_mut().for_each(|b| *b *= 0.7);
            }
        }
        let mut pixels = Vec::with_capacity(width * height * 4);
        for (a, b) in alpha.iter().zip(&bright) {
            let value = (b * 255.0).round() as u8;
            pixels.extend_from_slice(&[value, value, value, (a * 255.0).round() as u8]);
        }
        Rgba { width, height, pixels }
    }
}

struct Rgba {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

/// The names a face goes by (family, typographic family, full name - lower case, the family
/// first) and its full name.
fn face_names(names: ttf_parser::name::Names) -> (Vec<String>, String) {
    let mut family = None;
    let mut others = Vec::new();
    let mut full = String::new();
    for name in names {
        let Some(text) = name.to_string() else { continue };
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            continue;
        }
        match name.name_id {
            ttf_parser::name_id::FAMILY => {
                family.get_or_insert(text);
            }
            ttf_parser::name_id::TYPOGRAPHIC_FAMILY => others.push(text),
            ttf_parser::name_id::FULL_NAME => {
                if full.is_empty() {
                    full = text.clone();
                }
                others.push(text);
            }
            _ => {}
        }
    }
    let mut all: Vec<String> = family.into_iter().collect();
    for other in others {
        if !all.contains(&other) {
            all.push(other);
        }
    }
    (all, full)
}

/// The system's font folders.
fn system_font_folders() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if cfg!(windows) {
        let windows = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot")).map(PathBuf::from);
        folders.push(windows.unwrap_or_else(|| PathBuf::from("C:\\Windows")).join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            folders.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        folders.extend([PathBuf::from("/System/Library/Fonts"), PathBuf::from("/Library/Fonts")]);
        folders.extend(home.map(|h| h.join("Library").join("Fonts")));
    } else {
        folders.extend([PathBuf::from("/usr/share/fonts"), PathBuf::from("/usr/local/share/fonts")]);
        if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
            folders.push(PathBuf::from(data).join("fonts"));
        }
        if let Some(home) = home {
            folders.push(home.join(".local").join("share").join("fonts"));
            folders.push(home.join(".fonts"));
        }
    }
    folders
}

/// Font files in a folder and the folders in it.
fn font_files(folder: &Path, files: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if depth < 8 {
                font_files(&path, files, depth + 1);
            }
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ttf", "otf", "ttc"].iter().any(|x| e.eq_ignore_ascii_case(x)))
        {
            files.push(path);
        }
    }
}

type Description = (u32, (Vec<String>, String), (u16, bool));

/// The faces of a font file, from its name and OS/2 tables only - a font folder holds
/// hundreds of megabytes, and this reads a few kilobytes of each file.
fn describe_file(path: &Path) -> Vec<Description> {
    let Ok(mut file) = File::open(path) else { return Vec::new() };
    let read = |file: &mut File, at: u64, len: usize| -> Option<Vec<u8>> {
        let mut buffer = vec![0; len];
        file.seek(SeekFrom::Start(at)).ok()?;
        file.read_exact(&mut buffer).ok()?;
        Some(buffer)
    };
    let Some(header) = read(&mut file, 0, 12) else { return Vec::new() };
    let u32_at = |b: &[u8], at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let starts: Vec<u64> = if &header[..4] == b"ttcf" {
        let count = u32_at(&header, 8).min(256) as usize;
        match read(&mut file, 12, count * 4) {
            Some(offsets) => (0..count).map(|i| u64::from(u32_at(&offsets, i * 4))).collect(),
            None => Vec::new(),
        }
    } else {
        vec![0]
    };
    let mut faces = Vec::new();
    for (index, start) in starts.into_iter().enumerate() {
        let Some(head) = read(&mut file, start, 12) else { continue };
        let tables = usize::from(u16::from_be_bytes([head[4], head[5]])).min(512);
        let Some(directory) = read(&mut file, start + 12, tables * 16) else { continue };
        let table = |tag: &[u8; 4], file: &mut File| -> Option<Vec<u8>> {
            let record = directory.as_chunks::<16>().0.iter().find(|r| &r[..4] == tag)?;
            let (offset, len) = (u32_at(record, 8), u32_at(record, 12));
            if len > 4 << 20 {
                return None;
            }
            read(file, u64::from(offset), len as usize)
        };
        let Some(name) = table(b"name", &mut file) else { continue };
        let Some(names) = ttf_parser::name::Table::parse(&name) else { continue };
        let names = face_names(names.names);
        if names.0.is_empty() {
            continue;
        }
        let os2 = table(b"OS/2", &mut file);
        let style = os2
            .as_deref()
            .and_then(ttf_parser::os2::Table::parse)
            .map(|t| (t.weight().to_number(), t.style() != ttf_parser::Style::Normal))
            .unwrap_or((400, names.1.contains("italic")));
        faces.push((index as u32, names, style));
    }
    faces
}
