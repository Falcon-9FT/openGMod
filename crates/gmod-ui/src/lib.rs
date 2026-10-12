//! What Garry's Mod draws on the screen in 2D, before it reaches the GPU: the fonts of
//! `surface.CreateFont` ([`font`]) and the frame of shapes, images and text the HUD and the
//! panels are drawn from ([`canvas`]). The Lua side (`surface`, panels) fills a [`Canvas`];
//! the renderer draws it over the 3D view.

#![forbid(unsafe_code)]

pub mod canvas;
pub mod font;

pub use canvas::{Batch, Canvas, Clip, Texture, Vertex};
pub use font::{FontId, FontSpec, Fonts, Glyph};

#[cfg(test)]
mod tests;
