//! What the client's Lua draws in the world by itself, frame by frame: beams (`render.DrawBeam`,
//! `StartBeam` / `AddBeam` / `EndBeam`), sprites, quads and lines, each with the material set
//! (`render.SetMaterial`), coloured and faded as `SetColorModulation` and `SetBlend` say - and
//! the beams the engine draws itself: the physics gun's to what it holds.
//!
//! They are gathered while the frame is drawn ([`crate::ents::drawn`]) and handed to the
//! program ([`crate::Runtime::take_primitives`]), which turns them to the camera and draws them
//! over the world, after what is seen through.

use std::cell::{Cell, RefCell};

use mlua::{IntoLuaMulti, Lua, MultiValue, Table, Value};

use crate::args::Args;
use crate::host;
use crate::vector::{Angle, Vector};

/// What a primitive is drawn with: a material of the game, or one Lua made with its own base
/// texture.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Material3d {
    /// The material's name (`sprites/physbeam`), as a `.vmt` under `materials/`.
    pub name: String,
    /// The base texture Lua gave it (`CreateMaterial`), instead of the file's.
    pub texture: Option<String>,
    /// Added to what is behind it rather than blended over it, when Lua says.
    pub additive: Option<bool>,
}

/// A point of a beam: where, how wide, how far along its texture, its colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeamPoint {
    pub pos: Vector,
    pub width: f32,
    pub v: f32,
    /// Red, green, blue and alpha, from 0 to 1.
    pub color: [f32; 4],
}

/// A corner of a triangle Lua made (`mesh`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeshVertex {
    pub pos: Vector,
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// What is drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Triangles, three corners each, as they are.
    Triangles(Vec<MeshVertex>),
    /// A strip through its points, turned to face the camera.
    Beam(Vec<BeamPoint>),
    /// A rectangle facing the camera.
    Sprite { pos: Vector, width: f32, height: f32, color: [f32; 4] },
    /// Four corners, in order.
    Quad { corners: [Vector; 4], color: [f32; 4] },
}

/// One thing drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Primitive {
    pub material: Material3d,
    pub shape: Shape,
}

/// The primitives of the frame being drawn, and the state `render` keeps while it is.
#[derive(Default)]
pub(crate) struct Frame {
    primitives: RefCell<Vec<Primitive>>,
    material: RefCell<Material3d>,
    modulation: Cell<[f32; 3]>,
    blend: Cell<f32>,
    /// A beam being made (`StartBeam`): how many points it will have, and those it has.
    beam: RefCell<Option<(usize, Vec<BeamPoint>)>>,
    /// A mesh being made (`mesh.Begin`): its kind of primitive, the corners made, and the one
    /// being made.
    mesh: RefCell<Option<(i64, Vec<MeshVertex>, MeshVertex)>>,
    /// The planes 2D drawing goes on now (`cam.Start3D2D`), innermost last: each with the
    /// screen's drawing it took the place of.
    planes: RefCell<Vec<(Plane, gmod_ui::Canvas)>>,
}

/// A plane in the world 2D drawing is put on: where its `(0, 0)` is, which way its x and y go,
/// how long a unit of it is.
#[derive(Debug, Clone, Copy)]
struct Plane {
    origin: Vector,
    x: Vector,
    y: Vector,
}

/// The material of text drawn in the world: this, and the number of the page of the fonts'
/// glyphs ([`gmod_ui::Fonts::pages`]) it is cut from, which the program gives the renderer.
pub const GLYPHS: &str = "!glyphs/";

/// The material of a picture (`Material("x.png")`) drawn in the world: this, and the number of
/// its image ([`crate::Runtime::image`]), whose pixels the program gives the renderer.
pub const IMAGES: &str = "!image/";

// The canvas drawn on a plane: so big, and `(0, 0)` so far in, that nothing drawn is cut off at
// its edges, as drawing on a plane may go any way from its origin.
const PLANE_SIZE: u32 = 1 << 18;
const PLANE_ORIGIN: f32 = (1 << 17) as f32;

/// A frame begins: nothing drawn yet, the colour and fade as they are by default.
pub(crate) fn begin(lua: &Lua) {
    close_planes(lua);
    let frame = &host(lua).render3d;
    frame.primitives.borrow_mut().clear();
    frame.modulation.set([1.0; 3]);
    frame.blend.set(1.0);
    *frame.beam.borrow_mut() = None;
    *frame.mesh.borrow_mut() = None;
}

/// The frame's Lua is done: planes it left open (an error between `cam.Start3D2D` and
/// `cam.End3D2D`) are closed, so the screen is drawn on again.
pub(crate) fn close_planes(lua: &Lua) {
    while !host(lua).render3d.planes.borrow().is_empty() {
        end_3d2d(lua);
    }
}

/// `cam.Start3D2D(pos, angles, scale)`: what `surface` and `draw` draw from now on goes on a
/// plane in the world - x along the angles' forward, y along their right, `scale` units a
/// pixel - instead of on the screen.
fn start_3d2d(lua: &Lua, pos: Vector, angles: Angle, scale: f32) {
    let (forward, right, _) = angles.vectors();
    let plane = Plane { origin: pos, x: forward * scale, y: right * scale };
    let screen = &host(lua).screen;
    let mut canvas = gmod_ui::Canvas::new(PLANE_SIZE, PLANE_SIZE);
    canvas.origin = [PLANE_ORIGIN; 2];
    let taken = std::mem::replace(&mut *screen.canvas.borrow_mut(), canvas);
    host(lua).render3d.planes.borrow_mut().push((plane, taken));
}

/// `cam.End3D2D`: what was drawn on the plane put in the world, as triangles of the materials
/// it was drawn with - text of the page of the fonts' glyphs it was cut from
/// ([`GLYPHS`]) - and the screen is drawn on again.
fn end_3d2d(lua: &Lua) {
    let Some((plane, taken)) = host(lua).render3d.planes.borrow_mut().pop() else { return };
    let screen = &host(lua).screen;
    let drawn = std::mem::replace(&mut *screen.canvas.borrow_mut(), taken);
    for (material, triangles) in plane_triangles(&plane, &drawn, |id| screen.image(id)) {
        push_with(lua, material, Shape::Triangles(triangles));
    }
}

/// The triangles drawn on a plane where they are in the world, by the material of each batch.
fn plane_triangles(
    plane: &Plane,
    canvas: &gmod_ui::Canvas,
    image: impl Fn(u32) -> Option<crate::surface::Image>,
) -> Vec<(Material3d, Vec<MeshVertex>)> {
    let vertices = canvas.vertices();
    let mut out = Vec::new();
    for batch in canvas.batches() {
        let material = match batch.texture {
            gmod_ui::Texture::White => Material3d { name: "color".into(), texture: None, additive: Some(batch.additive) },
            gmod_ui::Texture::Image(id) => {
                let Some(found) = image(id) else { continue };
                let additive = Some(batch.additive || found.additive);
                // a picture (`Material("x.png")`) is no material of the game's: the program has
                // its pixels by its number
                let lower = found.name.to_ascii_lowercase();
                let picture = [".png", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e));
                let name = if picture { format!("{IMAGES}{id}") } else { found.name };
                Material3d { name, texture: None, additive }
            }
            gmod_ui::Texture::Glyphs(page) => Material3d { name: format!("{GLYPHS}{page}"), texture: None, additive: Some(batch.additive) },
        };
        let range = batch.start as usize..(batch.start + batch.count) as usize;
        let triangles = vertices[range]
            .iter()
            .map(|v| {
                let (x, y) = (v.pos[0] - PLANE_ORIGIN, v.pos[1] - PLANE_ORIGIN);
                MeshVertex { pos: plane.origin + plane.x * x + plane.y * y, uv: v.uv, color: v.color.map(|c| f32::from(c) / 255.0) }
            })
            .collect();
        out.push((material, triangles));
    }
    out
}

/// `cam.Start3D2D` and `cam.End3D2D`.
fn install_3d2d(lua: &Lua) -> mlua::Result<()> {
    let Some(cam) = lua.globals().raw_get::<Option<Table>>("cam")? else { return Ok(()) };
    cam.raw_set(
        "Start3D2D",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "Start3D2D");
            let pos = vector(&a, 1)?;
            let angles = a.userdata::<Angle>(2, "Angle")?;
            start_3d2d(lua, pos, angles, number(&a, 3, 1.0)?);
            Ok(())
        })?,
    )?;
    cam.raw_set(
        "End3D2D",
        lua.create_function(|lua, ()| {
            end_3d2d(lua);
            Ok(())
        })?,
    )
}

// the kinds of primitive `mesh.Begin` takes (`MATERIAL_*`)
const LINES: i64 = 1;
const TRIANGLES: i64 = 2;
const TRIANGLE_STRIP: i64 = 3;
const LINE_STRIP: i64 = 4;
const LINE_LOOP: i64 = 5;
const POLYGON: i64 = 6;
const QUADS: i64 = 7;

/// The corners of a mesh made into what is drawn: triangles of any kind, lines as thin beams.
fn mesh_shapes(kind: i64, v: &[MeshVertex]) -> Vec<Shape> {
    let tri = |list: &mut Vec<MeshVertex>, a: usize, b: usize, c: usize| list.extend([v[a], v[b], v[c]]);
    let mut triangles = Vec::new();
    match kind {
        TRIANGLES => (0..v.len() / 3).for_each(|i| tri(&mut triangles, 3 * i, 3 * i + 1, 3 * i + 2)),
        TRIANGLE_STRIP => (2..v.len()).for_each(|i| tri(&mut triangles, i - 2, i - 1, i)),
        POLYGON => (2..v.len()).for_each(|i| tri(&mut triangles, 0, i - 1, i)),
        QUADS => (0..v.len() / 4).for_each(|i| {
            tri(&mut triangles, 4 * i, 4 * i + 1, 4 * i + 2);
            tri(&mut triangles, 4 * i, 4 * i + 2, 4 * i + 3);
        }),
        LINES | LINE_STRIP | LINE_LOOP => {
            let pairs: Vec<(usize, usize)> = match kind {
                LINES => (0..v.len() / 2).map(|i| (2 * i, 2 * i + 1)).collect(),
                LINE_STRIP => (1..v.len()).map(|i| (i - 1, i)).collect(),
                _ => (0..v.len()).map(|i| (i, (i + 1) % v.len())).collect(),
            };
            return pairs
                .into_iter()
                .map(|(a, b)| {
                    let point = |m: &MeshVertex, t: f32| BeamPoint { pos: m.pos, width: 0.5, v: t, color: m.color };
                    Shape::Beam(vec![point(&v[a], 0.0), point(&v[b], 1.0)])
                })
                .collect();
        }
        _ => {}
    }
    if triangles.is_empty() { Vec::new() } else { vec![Shape::Triangles(triangles)] }
}

/// `mesh`: corners one by one (`Begin`, `Position`, `TexCoord`, `Color`, `AdvanceVertex`,
/// `End`), drawn with the material set; and quads in a call (`Quad`, `QuadEasy`).
fn install_mesh(lua: &Lua) -> mlua::Result<()> {
    let mesh = lua.create_table()?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        mesh.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    // `Begin(kind, count)`; one made into an `IMesh` (`Begin(mesh, kind, count)`) is not kept
    def("Begin", |a| {
        let kind = match a.get(1) {
            Some(Value::Integer(n)) => Some(*n),
            Some(Value::Number(n)) => Some(*n as i64),
            _ => None,
        };
        *host(a.lua).render3d.mesh.borrow_mut() = kind.map(|k| (k, Vec::new(), MeshVertex { color: [1.0; 4], ..MeshVertex::default() }));
        Ok(Value::Nil)
    })?;
    mesh.raw_set(
        "Position",
        lua.create_function(move |lua, values: MultiValue| {
            let a = Args::new(lua, values, "Position");
            let pos = vector(&a, 1)?;
            if let Some((_, _, vertex)) = host(lua).render3d.mesh.borrow_mut().as_mut() {
                vertex.pos = pos;
            }
            Ok(())
        })?,
    )?;
    def("TexCoord", |a| {
        let (stage, u, v) = (a.opt_number(1)?.unwrap_or(0.0), number(a, 2, 0.0)?, number(a, 3, 0.0)?);
        if stage == 0.0
            && let Some((_, _, vertex)) = host(a.lua).render3d.mesh.borrow_mut().as_mut()
        {
            vertex.uv = [u, v];
        }
        Ok(Value::Nil)
    })?;
    def("Color", |a| {
        let rgba = [number(a, 1, 255.0)?, number(a, 2, 255.0)?, number(a, 3, 255.0)?, number(a, 4, 255.0)?].map(|c| c / 255.0);
        if let Some((_, _, vertex)) = host(a.lua).render3d.mesh.borrow_mut().as_mut() {
            vertex.color = rgba;
        }
        Ok(Value::Nil)
    })?;
    def("AdvanceVertex", |a| {
        if let Some((_, list, vertex)) = host(a.lua).render3d.mesh.borrow_mut().as_mut() {
            list.push(*vertex);
        }
        Ok(Value::Nil)
    })?;
    def("End", |a| {
        let made = host(a.lua).render3d.mesh.borrow_mut().take();
        if let Some((kind, list, _)) = made {
            for shape in mesh_shapes(kind, &list) {
                push(a.lua, shape);
            }
        }
        Ok(Value::Nil)
    })?;
    def("VertexCount", |a| {
        let count = host(a.lua).render3d.mesh.borrow().as_ref().map_or(0, |(_, list, _)| list.len());
        Ok(Value::Integer(count as i64))
    })?;
    // what the renderer has no use for: normals, tangents, the data of shaders
    for name in ["Normal", "TangentS", "TangentT", "UserData", "Specular", "BoneData"] {
        def(name, |_| Ok(Value::Nil))?;
    }
    def("Quad", |a| {
        let corners = [vector(a, 1)?, vector(a, 2)?, vector(a, 3)?, vector(a, 4)?];
        push(a.lua, Shape::Quad { corners, color: color(a, 5) });
        Ok(Value::Nil)
    })?;
    def("QuadEasy", |a| {
        let (pos, normal) = (vector(a, 1)?, vector(a, 2)?);
        let (width, height) = (number(a, 3, 1.0)?, number(a, 4, 1.0)?);
        let (_, right, up) = normal.angle().vectors();
        let (r, u) = (right * (width / 2.0), up * (height / 2.0));
        push(a.lua, Shape::Quad { corners: [pos - r + u, pos + r + u, pos + r - u, pos - r - u], color: color(a, 5) });
        Ok(Value::Nil)
    })?;
    lua.globals().raw_set("mesh", mesh)
}

/// `util.GetPixelVisibleHandle` and `util.PixelVisible`: whether a point can be seen now - in
/// front of the eyes, nothing solid between them and it (1, else 0); for glows and flares.
fn install_visibility(lua: &Lua) -> mlua::Result<()> {
    let Some(util) = lua.globals().raw_get::<Option<Table>>("util")? else { return Ok(()) };
    util.raw_set("GetPixelVisibleHandle", lua.create_function(|lua, ()| lua.create_table())?)?;
    util.raw_set(
        "PixelVisible",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "PixelVisible");
            let point = vector(&a, 1)?;
            let radius = number(&a, 2, 0.0)?;
            let view = host(lua).client.view.get();
            let to = point - view.origin;
            if to.dot(view.angles.vectors().0) <= 0.0 {
                return Ok(0.0);
            }
            let player = host(lua).client.local_player.get();
            let sweep = crate::trace::Sweep::line(view.origin, point, crate::trace::Filter::Entities(vec![player]));
            let hit = crate::trace::trace(lua, &sweep);
            let length = to.length().max(1e-3);
            Ok(if hit.fraction >= 1.0 || (1.0 - hit.fraction) * length <= radius.max(1.0) { 1.0 } else { 0.0 })
        })?,
    )
}

/// Something drawn now, with the material set.
pub(crate) fn push(lua: &Lua, shape: Shape) {
    let frame = &host(lua).render3d;
    let material = frame.material.borrow().clone();
    frame.primitives.borrow_mut().push(Primitive { material, shape });
}

/// Something drawn with a material of its own (the engine's own beams).
pub(crate) fn push_with(lua: &Lua, material: Material3d, shape: Shape) {
    host(lua).render3d.primitives.borrow_mut().push(Primitive { material, shape });
}

/// A `Color` table (0 to 255), modulated and faded as `render` says now; white by default.
fn color(a: &Args, position: usize) -> [f32; 4] {
    let frame = &host(a.lua).render3d;
    let [r, g, b] = frame.modulation.get();
    let blend = frame.blend.get();
    let channel = |t: &Table, key: &str, default: f32| t.get::<Option<f64>>(key).ok().flatten().map_or(default, |v| v as f32 / 255.0);
    match a.get(position) {
        Some(Value::Table(t)) => {
            [channel(t, "r", 1.0) * r, channel(t, "g", 1.0) * g, channel(t, "b", 1.0) * b, channel(t, "a", 1.0) * blend]
        }
        _ => [r, g, b, blend],
    }
}

fn vector(a: &Args, position: usize) -> mlua::Result<Vector> {
    a.userdata::<Vector>(position, "Vector")
}

fn number(a: &Args, position: usize, default: f64) -> mlua::Result<f32> {
    Ok(a.opt_number(position)?.unwrap_or(default) as f32)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    install_mesh(lua)?;
    install_visibility(lua)?;
    install_3d2d(lua)?;
    let Some(render) = lua.globals().raw_get::<Option<Table>>("render")? else { return Ok(()) };
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        render.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("SetMaterial", |a| {
        let material = match a.get(1) {
            Some(Value::UserData(ud)) => crate::material::render_info(ud),
            _ => None,
        };
        *host(a.lua).render3d.material.borrow_mut() = material.unwrap_or_default();
        Ok(Value::Nil)
    })?;
    // a plain colour: the engine's `color` material
    let plain = |a: &Args| {
        *host(a.lua).render3d.material.borrow_mut() = Material3d { name: "color".into(), texture: None, additive: Some(false) };
        Ok(Value::Nil)
    };
    def("SetColorMaterial", plain)?;
    def("SetColorMaterialIgnoreZ", plain)?;
    def("SetColorModulation", |a| {
        let rgb = [number(a, 1, 1.0)?, number(a, 2, 1.0)?, number(a, 3, 1.0)?];
        host(a.lua).render3d.modulation.set(rgb);
        Ok(Value::Nil)
    })?;
    render.raw_set(
        "GetColorModulation",
        lua.create_function(|lua, _: MultiValue| {
            let [r, g, b] = host(lua).render3d.modulation.get();
            (r, g, b).into_lua_multi(lua)
        })?,
    )?;
    def("SetBlend", |a| {
        host(a.lua).render3d.blend.set(number(a, 1, 1.0)?.clamp(0.0, 1.0));
        Ok(Value::Nil)
    })?;
    def("GetBlend", |a| Ok(Value::Number(f64::from(host(a.lua).render3d.blend.get()))))?;
    // start, end, width, texture start, texture end, colour
    def("DrawBeam", |a| {
        let (start, end, width) = (vector(a, 1)?, vector(a, 2)?, number(a, 3, 1.0)?);
        let (from, to) = (number(a, 4, 0.0)?, number(a, 5, 1.0)?);
        let color = color(a, 6);
        let point = |pos, v| BeamPoint { pos, width, v, color };
        push(a.lua, Shape::Beam(vec![point(start, from), point(end, to)]));
        Ok(Value::Nil)
    })?;
    def("StartBeam", |a| {
        let count = a.number(1)?.max(0.0) as usize;
        *host(a.lua).render3d.beam.borrow_mut() = Some((count, Vec::with_capacity(count)));
        Ok(Value::Nil)
    })?;
    def("AddBeam", |a| {
        let point = BeamPoint { pos: vector(a, 1)?, width: number(a, 2, 1.0)?, v: number(a, 3, 0.0)?, color: color(a, 4) };
        if let Some((_, points)) = host(a.lua).render3d.beam.borrow_mut().as_mut() {
            points.push(point);
        }
        Ok(Value::Nil)
    })?;
    def("EndBeam", |a| {
        let beam = host(a.lua).render3d.beam.borrow_mut().take();
        if let Some((_, points)) = beam.filter(|(_, points)| points.len() >= 2) {
            push(a.lua, Shape::Beam(points));
        }
        Ok(Value::Nil)
    })?;
    def("DrawSprite", |a| {
        let shape = Shape::Sprite { pos: vector(a, 1)?, width: number(a, 2, 1.0)?, height: number(a, 3, 1.0)?, color: color(a, 4) };
        push(a.lua, shape);
        Ok(Value::Nil)
    })?;
    def("DrawQuad", |a| {
        let corners = [vector(a, 1)?, vector(a, 2)?, vector(a, 3)?, vector(a, 4)?];
        push(a.lua, Shape::Quad { corners, color: color(a, 5) });
        Ok(Value::Nil)
    })?;
    // a rectangle about a point, facing along a normal, turned about it
    def("DrawQuadEasy", |a| {
        let (pos, normal) = (vector(a, 1)?, vector(a, 2)?);
        let (width, height) = (number(a, 3, 1.0)?, number(a, 4, 1.0)?);
        let color = color(a, 5);
        let rotation = number(a, 6, 0.0)?;
        let mut angles = normal.angle();
        angles.r += rotation;
        let (_, right, up) = Angle::new(angles.p, angles.y, angles.r).vectors();
        let (r, u) = (right * (width / 2.0), up * (height / 2.0));
        push(a.lua, Shape::Quad { corners: [pos - r + u, pos + r + u, pos + r - u, pos - r - u], color });
        Ok(Value::Nil)
    })?;
    // a line: a thin beam of a plain colour
    def("DrawLine", |a| {
        let (start, end) = (vector(a, 1)?, vector(a, 2)?);
        let color = color(a, 3);
        let material = Material3d { name: "color".into(), texture: None, additive: Some(false) };
        let point = |pos, v| BeamPoint { pos, width: 0.5, v, color };
        push_with(a.lua, material, Shape::Beam(vec![point(start, 0.0), point(end, 1.0)]));
        Ok(Value::Nil)
    })?;
    Ok(())
}

/// The physics guns' beams, as GMod draws them: from the gun's muzzle - the view model's for
/// the one who looks through it, where it shows - bending from where the gun aims to what it
/// holds, or straight to where it aims while nothing is held; the texture runs along it, a
/// glow at either end, in the colour of the player's weapon (`GetWeaponColor`). Only a gun in
/// its owner's hands draws one.
pub(crate) fn physgun_beams(lua: &Lua, viewer: crate::entity::EntityHandle) {
    use crate::entity::EntityHandle;
    let guns: Vec<(EntityHandle, EntityHandle, EntityHandle, Vector)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                let (target, point) = ent.beam?;
                (!ent.removed && !ent.dormant).then_some((h, ent.owner, target, point))
            })
            .collect()
    };
    let time = host(lua).started.elapsed().as_secs_f32();
    for (gun, owner, target, point) in guns {
        if crate::player::active_weapon(lua, owner) != Some(gun) {
            continue;
        }
        // the eyes and the aim: the view's for the viewer, which moves smoothly between ticks
        let (eye, aim) = if owner == viewer {
            let view = host(lua).client.view.get();
            (view.origin, view.angles.vectors().0)
        } else {
            let Some(found) = crate::player::eye(lua, owner) else { continue };
            found
        };
        let start = if owner == viewer {
            crate::anim::view_model_attachment(lua, owner, &["muzzle", "core"])
        } else {
            crate::anim::named_attachment(lua, gun, &["muzzle", "core"])
        };
        let start = start.unwrap_or_else(|| {
            let (_, right, up) = aim.angle().vectors();
            eye + aim * 24.0 + right * 6.0 - up * 7.0
        });
        let held = host(lua).entities.is_valid(target) && target.index.is_some_and(|i| i != 0);
        let end = if held {
            let Some((pos, angles)) = host(lua).entities.transform(target) else { continue };
            pos + point.rotated(angles)
        } else {
            let range = crate::convar::value(lua, "physgun_maxrange").and_then(|v| v.trim().parse().ok()).unwrap_or(4096.0f32);
            let sweep = crate::trace::Sweep {
                mask: gmod_bsp::mask::SHOT,
                ..crate::trace::Sweep::line(eye, eye + aim * range, crate::trace::Filter::Entities(vec![owner]))
            };
            crate::trace::trace(lua, &sweep).end
        };
        let colour = crate::player::state(lua, owner, |_, p| p.weapon_color).unwrap_or(Vector::new(0.3, 1.8, 2.1));
        let rgb = [colour.x, colour.y, colour.z].map(|c| c.clamp(0.0, 1.0));
        let color = [rgb[0], rgb[1], rgb[2], 1.0];
        // bending: it leaves the gun along the aim and comes round to what it holds
        let length = (end - start).length();
        let control = start + aim * (length * 0.5);
        const STEPS: usize = 24;
        let scroll = (time * 2.0).fract();
        let points = (0..=STEPS)
            .map(|i| {
                let t = i as f32 / STEPS as f32;
                let pos = start * ((1.0 - t) * (1.0 - t)) + control * (2.0 * t * (1.0 - t)) + end * (t * t);
                BeamPoint { pos, width: 3.0, v: t * (length / 128.0).max(1.0) - scroll, color }
            })
            .collect();
        let additive = |name: &str| Material3d { name: name.into(), texture: None, additive: Some(true) };
        push_with(lua, additive("sprites/physbeama"), Shape::Beam(points));
        push_with(lua, additive("sprites/physg_glow1"), Shape::Sprite { pos: start, width: 6.0, height: 6.0, color });
        let size = if held { 16.0 } else { 10.0 };
        push_with(lua, additive("sprites/physg_glow2"), Shape::Sprite { pos: end, width: size, height: size, color });
    }
}

impl crate::Runtime {
    /// What was drawn in the world from Lua (and the engine's beams) in the frame just drawn
    /// ([`crate::Runtime::drawn`]).
    pub fn take_primitives(&self) -> Vec<Primitive> {
        std::mem::take(&mut *host(&self.lua).render3d.primitives.borrow_mut())
    }
}
