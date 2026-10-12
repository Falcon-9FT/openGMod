//! A frame of 2D drawing: what `surface` draws for the HUD and the panels, in pixels from the
//! top left corner of the screen, as one list of triangles in batches by texture - for the GPU
//! to draw in one pass, in order, over the 3D view.
//!
//! Rectangles, images and text are clipped here, against the rectangle panels draw in, so
//! that batches do not break at every panel; what turns (a rotated image, a polygon, a line
//! across) carries its clip to the GPU, which cuts it there.

use crate::font::{FontId, Fonts};

/// What a batch draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Texture {
    /// Plain colour.
    White,
    /// A page of the font atlas ([`Fonts::pages`]).
    Glyphs(u32),
    /// An image (a material, a texture) the program loads by number.
    Image(u32),
}

/// A rectangle drawing is kept in, in whole pixels: the right and bottom edges are outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clip {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Clip {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Clip {
        Clip { left: x, top: y, right: x + width.max(0), bottom: y + height.max(0) }
    }

    /// Where both are.
    pub fn intersect(self, other: Clip) -> Clip {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        Clip { left, top, right: self.right.min(other.right).max(left), bottom: self.bottom.min(other.bottom).max(top) }
    }

    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// A corner of a triangle: where on the screen, where in the texture, its colour.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

impl Vertex {
    pub const SIZE: usize = 20;

    pub fn new(x: f32, y: f32, u: f32, v: f32, color: [u8; 4]) -> Vertex {
        Vertex { pos: [x, y], uv: [u, v], color }
    }

    /// As the GPU reads it: two floats, two floats, four bytes.
    pub fn bytes(&self, out: &mut Vec<u8>) {
        for value in [self.pos[0], self.pos[1], self.uv[0], self.uv[1]] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&self.color);
    }
}

/// Triangles drawn the same way: `count` vertices from `start`, three a triangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Batch {
    pub texture: Texture,
    /// Adding light instead of covering.
    pub additive: bool,
    /// What the GPU cuts to; none for the whole screen.
    pub clip: Option<Clip>,
    pub start: u32,
    pub count: u32,
}

/// One frame of 2D drawing.
#[derive(Debug, Clone, Default)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    vertices: Vec<Vertex>,
    batches: Vec<Batch>,
    /// Drawing is kept in this rectangle of the screen; none: the whole screen.
    pub clip: Option<Clip>,
    /// Where `(0, 0)` is on the screen: a panel's top left corner while it paints.
    pub origin: [f32; 2],
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas { width, height, ..Canvas::default() }
    }

    /// Empty, for a frame of this size.
    pub fn reset(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.vertices.clear();
        self.batches.clear();
        self.clip = None;
        self.origin = [0.0; 2];
    }

    pub fn vertices(&self) -> &[Vertex] {
        &self.vertices
    }

    pub fn batches(&self) -> &[Batch] {
        &self.batches
    }

    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// The clip, and the screen.
    fn bounds(&self) -> Clip {
        let screen = Clip::new(0, 0, self.width as i32, self.height as i32);
        self.clip.map_or(screen, |c| c.intersect(screen))
    }

    /// Triangles, three vertices each.
    pub fn triangles(&mut self, texture: Texture, additive: bool, clip: Option<Clip>, vertices: &[Vertex]) {
        if vertices.is_empty() {
            return;
        }
        let start = self.vertices.len() as u32;
        self.vertices.extend_from_slice(vertices);
        if let Some(last) = self.batches.last_mut()
            && last.texture == texture
            && last.additive == additive
            && last.clip == clip
            && last.start + last.count == start
        {
            last.count += vertices.len() as u32;
            return;
        }
        self.batches.push(Batch { texture, additive, clip, start, count: vertices.len() as u32 });
    }

    /// A rectangle of a texture, upright, cut to the clip here: `uv` is its left, top, right
    /// and bottom in the texture.
    pub fn image(&mut self, texture: Texture, additive: bool, rect: [f32; 4], uv: [f32; 4], color: [u8; 4]) {
        let [x, y, w, h] = rect;
        let (x, y) = (x + self.origin[0], y + self.origin[1]);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let bounds = self.bounds();
        let (x0, y0) = (x.max(bounds.left as f32), y.max(bounds.top as f32));
        let (x1, y1) = ((x + w).min(bounds.right as f32), (y + h).min(bounds.bottom as f32));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let u = |px: f32| uv[0] + (uv[2] - uv[0]) * (px - x) / w;
        let v = |py: f32| uv[1] + (uv[3] - uv[1]) * (py - y) / h;
        let corners = [
            Vertex::new(x0, y0, u(x0), v(y0), color),
            Vertex::new(x1, y0, u(x1), v(y0), color),
            Vertex::new(x1, y1, u(x1), v(y1), color),
            Vertex::new(x0, y1, u(x0), v(y1), color),
        ];
        self.quad(texture, additive, None, corners);
    }

    /// A rectangle of colour.
    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: [u8; 4]) {
        self.image(Texture::White, false, [x, y, width, height], [0.0, 0.0, 1.0, 1.0], color);
    }

    /// The edges of a rectangle, `thickness` pixels inside it.
    pub fn outlined_rect(&mut self, x: f32, y: f32, width: f32, height: f32, thickness: f32, color: [u8; 4]) {
        let t = thickness.max(1.0).min(width / 2.0).min(height / 2.0);
        self.rect(x, y, width, t, color);
        self.rect(x, y + height - t, width, t, color);
        self.rect(x, y + t, t, height - 2.0 * t, color);
        self.rect(x + width - t, y + t, t, height - 2.0 * t, color);
    }

    /// Four corners in order round the quad.
    fn quad(&mut self, texture: Texture, additive: bool, clip: Option<Clip>, c: [Vertex; 4]) {
        self.triangles(texture, additive, clip, &[c[0], c[1], c[2], c[0], c[2], c[3]]);
    }

    /// The clip to give the GPU for what turns: none when it is the whole screen.
    fn gpu_clip(&self) -> Option<Clip> {
        let bounds = self.bounds();
        if bounds == Clip::new(0, 0, self.width as i32, self.height as i32) { None } else { Some(bounds) }
    }

    /// A rectangle of a texture `width` by `height` centred on `(x, y)`, turned `degrees`
    /// anticlockwise, as `surface.DrawTexturedRectRotated` draws it.
    pub fn rotated(&mut self, texture: Texture, additive: bool, centre: [f32; 2], size: [f32; 2], degrees: f32, color: [u8; 4]) {
        let (sin, cos) = (-degrees).to_radians().sin_cos();
        let centre = [centre[0] + self.origin[0], centre[1] + self.origin[1]];
        let (hw, hh) = (size[0] / 2.0, size[1] / 2.0);
        let corner =
            |dx: f32, dy: f32, u: f32, v: f32| Vertex::new(centre[0] + dx * cos - dy * sin, centre[1] + dx * sin + dy * cos, u, v, color);
        let corners = [corner(-hw, -hh, 0.0, 0.0), corner(hw, -hh, 1.0, 0.0), corner(hw, hh, 1.0, 1.0), corner(-hw, hh, 0.0, 1.0)];
        let clip = self.gpu_clip();
        self.quad(texture, additive, clip, corners);
    }

    /// A line one pixel wide, from the centre of one pixel to the centre of another.
    pub fn line(&mut self, from: [f32; 2], to: [f32; 2], color: [u8; 4]) {
        let [ox, oy] = self.origin;
        let (x0, y0, x1, y1) = (from[0] + ox + 0.5, from[1] + oy + 0.5, to[0] + ox + 0.5, to[1] + oy + 0.5);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let length = (dx * dx + dy * dy).sqrt();
        if length == 0.0 {
            self.rect(from[0], from[1], 1.0, 1.0, color);
            return;
        }
        let (nx, ny) = (-dy / length * 0.5, dx / length * 0.5);
        let corners = [
            Vertex::new(x0 + nx, y0 + ny, 0.0, 0.0, color),
            Vertex::new(x1 + nx, y1 + ny, 1.0, 0.0, color),
            Vertex::new(x1 - nx, y1 - ny, 1.0, 1.0, color),
            Vertex::new(x0 - nx, y0 - ny, 0.0, 1.0, color),
        ];
        let clip = self.gpu_clip();
        self.quad(Texture::White, false, clip, corners);
    }

    /// A convex polygon of a texture: `(x, y, u, v)` round it.
    pub fn polygon(&mut self, texture: Texture, additive: bool, points: &[[f32; 4]], color: [u8; 4]) {
        if points.len() < 3 {
            return;
        }
        let [ox, oy] = self.origin;
        let vertex = |p: &[f32; 4]| Vertex::new(p[0] + ox, p[1] + oy, p[2], p[3], color);
        let mut triangles = Vec::with_capacity((points.len() - 2) * 3);
        for pair in points[1..].windows(2) {
            triangles.extend([vertex(&points[0]), vertex(&pair[0]), vertex(&pair[1])]);
        }
        let clip = self.gpu_clip();
        self.triangles(texture, additive, clip, &triangles);
    }

    /// Text in a font, its line's top left corner at `(x, y)`: how far it went to the right.
    /// A line break goes back to `x`, a line lower.
    pub fn text(&mut self, fonts: &mut Fonts, font: FontId, x: f32, y: f32, text: &str, color: [u8; 4]) -> f32 {
        let additive = fonts.spec(font).additive;
        let tall = fonts.tall(font) as f32;
        let (mut pen, mut line) = (x.round(), y.round());
        let mut widest: f32 = 0.0;
        for ch in text.chars() {
            if ch == '\n' {
                widest = widest.max(pen - x);
                pen = x.round();
                line += tall;
                continue;
            }
            let glyph = fonts.glyph(font, ch);
            if glyph.width > 0 {
                let rect = [pen + glyph.left as f32, line + glyph.top as f32, glyph.width as f32, glyph.height as f32];
                self.image(Texture::Glyphs(glyph.page), additive, rect, glyph.uv, color);
            }
            pen += glyph.advance as f32;
        }
        widest.max(pen - x)
    }
}
