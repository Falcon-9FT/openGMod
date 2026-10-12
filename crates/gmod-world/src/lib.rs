//! A map ready to draw - what a renderer needs, from the GPU one to the CPU preview - and to
//! walk in: what is solid ([`collision`]).
//!
//! [`load`] turns a map into [`Piece`]s - the world, every brush entity that can be seen
//! (`func_brush`, doors ...) moved to where it stands, and every static prop with its model -
//! each a list of triangles grouped into [`Surface`]s by material. Brushes are lit by their
//! light maps, props by the light baked into the map for each of their vertices. Alongside
//! come the textures the materials use, still as VTF files (a GPU takes DXT as it is), the
//! sky box, and the fog. What stands in the room of the 3D sky box is kept apart from the
//! world, to be drawn behind it ([`sky`]).
//!
//! The map's pakfile holds materials, models and the props' light: mount it in front first
//! ([`mount_pakfile`]), as the engine does while the map is played. [`gpu`] lays the world out
//! for a GPU.
//!
//! What is solid is the map's brushes and displacements, and its static props by their
//! physics models (`.phy`) or by the box of their hull, as each prop says.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use gmod_bsp::collision::{Collision, Hull};
use gmod_bsp::props::solid;
use gmod_bsp::{Bsp, mesh};
use gmod_fs::{Backing, Mount, Vfs};
use gmod_material::{Library, Material, Vtf};
use gmod_model::{Mdl, Model, Vhv};

pub mod gpu;
pub mod light;
pub mod sky;

pub use gmod_bsp::mesh::Lightmap;

pub type Vec3 = [f32; 3];

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalize(a: Vec3) -> Vec3 {
    let length = dot(a, a).sqrt();
    if length > 0.0 { scale(a, 1.0 / length) } else { a }
}

/// A vertex, in the world.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    /// Texture coordinates: 1 is the whole texture.
    pub uv: [f32; 2],
    /// Coordinates in the piece's light map (0 to 1); unused for props.
    pub lightmap_uv: [f32; 2],
    /// How much of a blended material's second texture shows (displacements).
    pub alpha: f32,
    /// Linear light at the vertex, for props; 1 for brushes, lit by their light map.
    pub light: Vec3,
}

/// Triangles that share a material, and how to draw them.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    /// The material's name as the map or model gives it.
    pub material: String,
    /// The triangles: `count` indices of the piece from `first` on.
    pub first: usize,
    pub count: usize,
    /// Index into [`World::textures`]: the base texture, `None` when it is missing.
    pub base: Option<usize>,
    /// The second texture of `WorldVertexTransition`, blended in by vertex alpha.
    pub base2: Option<usize>,
    /// Texels less opaque than this are not drawn.
    pub alpha_test: Option<f32>,
    pub translucent: bool,
    pub additive: bool,
    /// `$nofog`: fog does not cover it (glows and the like).
    pub no_fog: bool,
    /// Lit by light (map or vertex); `UnlitGeneric` and water are not.
    pub lit: bool,
    /// Drawn from both sides.
    pub no_cull: bool,
    /// `$color`: multiplies the texture.
    pub tint: Vec3,
    /// `$alpha`.
    pub alpha: f32,
    /// A water surface and its fog colour.
    pub water: Option<Vec3>,
    /// The material was not found: the game shows purple and black squares.
    pub missing: bool,
}

impl Surface {
    fn plain(material: &str, first: usize, count: usize) -> Surface {
        Surface {
            material: material.to_string(),
            first,
            count,
            base: None,
            base2: None,
            alpha_test: None,
            translucent: false,
            additive: false,
            no_fog: false,
            lit: true,
            no_cull: false,
            tint: [1.0; 3],
            alpha: 1.0,
            water: None,
            missing: true,
        }
    }
}

/// The bones that move a vertex of a model, and how much each: up to three, the weights adding
/// up to 1.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Weights {
    pub bones: [u8; 3],
    pub weights: [f32; 3],
}

/// Something drawn: the world, a brush entity or a prop, moved to where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// The light map of a brush model; props have vertex light instead.
    pub lightmap: Option<mesh::Lightmap>,
    pub surfaces: Vec<Surface>,
    /// For a model that moves, the bones of each vertex; empty for what stands still.
    pub skin: Vec<Weights>,
}

/// A texture a material uses.
#[derive(Debug, Clone)]
pub struct Texture {
    /// As materials name it (`concrete/concretefloor001a`).
    pub name: String,
    pub vtf: Arc<Vtf>,
}

/// A texture coordinate transform (`$basetexturetransform`): `center`, `scale`, `rotate`
/// (degrees), `translate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub center: [f32; 2],
    pub scale: [f32; 2],
    pub rotate: f32,
    pub translate: [f32; 2],
}

impl Transform {
    pub fn parse(text: &str) -> Option<Transform> {
        let mut t = Transform { center: [0.5, 0.5], scale: [1.0, 1.0], rotate: 0.0, translate: [0.0, 0.0] };
        let words: Vec<&str> = text.split_whitespace().collect();
        let number = |i: usize| words.get(i).and_then(|w| w.parse::<f32>().ok());
        let mut i = 0;
        while i < words.len() {
            match words[i].to_ascii_lowercase().as_str() {
                "center" => t.center = [number(i + 1)?, number(i + 2)?],
                "scale" => t.scale = [number(i + 1)?, number(i + 2)?],
                "translate" => t.translate = [number(i + 1)?, number(i + 2)?],
                "rotate" => {
                    t.rotate = number(i + 1)?;
                    i += 2;
                    continue;
                }
                _ => return None,
            }
            i += 3;
        }
        Some(t)
    }

    pub fn apply(&self, uv: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.rotate.to_radians().sin_cos();
        let p = [(uv[0] - self.center[0]) * self.scale[0], (uv[1] - self.center[1]) * self.scale[1]];
        let p = [p[0] * c - p[1] * s, p[0] * s + p[1] * c];
        [p[0] + self.center[0] + self.translate[0], p[1] + self.center[1] + self.translate[1]]
    }
}

/// The six faces of the sky box, as the names of its textures end, for the directions +X, -X,
/// +Y, -Y, +Z and -Z.
pub const SKY_FACES: [&str; 6] = ["rt", "lf", "bk", "ft", "up", "dn"];

/// The face of the sky box seen in direction `d`, and the texture coordinates on it.
pub fn sky_face(d: Vec3) -> (usize, [f32; 2]) {
    let (ax, ay, az) = (d[0].abs(), d[1].abs(), d[2].abs());
    let (face, s, t) = if ax >= ay && ax >= az {
        if d[0] > 0.0 { (0, -d[1] / ax, d[2] / ax) } else { (1, d[1] / ax, d[2] / ax) }
    } else if ay >= az {
        if d[1] > 0.0 { (2, d[0] / ay, d[2] / ay) } else { (3, -d[0] / ay, d[2] / ay) }
    } else if d[2] > 0.0 {
        (4, -d[1] / az, -d[0] / az)
    } else {
        (5, -d[1] / az, d[0] / az)
    };
    (face, [(s + 1.0) / 2.0, (1.0 - t) / 2.0])
}

/// One face of the sky box: its texture and the texture transform of its material.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyFace {
    pub texture: usize,
    pub transform: Option<Transform>,
}

/// A map ready to draw.
#[derive(Debug, Clone)]
pub struct World {
    pub pieces: Vec<Piece>,
    pub textures: Vec<Texture>,
    /// The sky box faces, in the order of [`SKY_FACES`].
    pub sky: [Option<SkyFace>; 6],
    /// The 3D sky box, drawn between the sky box and the world ([`sky`]).
    pub skybox: Option<sky::Skybox>,
    /// The world's fog.
    pub fog: Option<sky::Fog>,
    /// What the map needs but the game does not have.
    pub missing_materials: Vec<String>,
    pub missing_textures: Vec<String>,
    pub missing_models: Vec<String>,
}

impl World {
    /// The triangles drawn: the world's and the 3D sky box's.
    pub fn triangles(&self) -> usize {
        let pieces = self.pieces.iter().chain(self.skybox.iter().flat_map(|s| &s.pieces));
        pieces.map(|p| p.indices.len() / 3).sum()
    }
}

/// Mounts a map's pakfile in front of the other mounts, with search paths `GAME` and `BSP`.
/// Returns whether there was one; take it out with `vfs.remove(0)` when the map is left.
pub fn mount_pakfile(vfs: &mut Vfs, bsp: &Bsp) -> io::Result<bool> {
    let pak = bsp.pakfile()?;
    let mounted = pak.is_some();
    if let Some(pak) = pak {
        vfs.insert(0, Mount::new(&["GAME", "BSP"], Backing::Zip(pak)));
    }
    Ok(mounted)
}

/// The entities a view starts from, in the order they are looked for: where players start, in
/// the games whose maps GMod plays, and the points spectators look from.
const STARTS: [&str; 9] = [
    "info_player_start",
    "gmod_player_start",
    "info_observer_point",
    "info_player_teamspawn",
    "info_player_deathmatch",
    "info_player_counterterrorist",
    "info_player_terrorist",
    "info_player_combine",
    "info_player_rebel",
];

/// Where a player starts: the first spawn point of the kinds [`STARTS`] lists. Returns its
/// origin, which is where the feet are, and its angles.
pub fn spawn_point(bsp: &Bsp) -> Option<(Vec3, Vec3)> {
    STARTS.iter().filter(|class| **class != "info_observer_point").find_map(|class| {
        let entity = bsp.entities.iter().find(|e| e.class_name() == *class)?;
        Some((entity.vec3("origin")?, entity.vec3("angles").unwrap_or([0.0; 3])))
    })
}

/// Where to look from when nothing else says: a player start or an observer point, else above
/// the middle of the world. Returns the eye and a point it looks at.
pub fn default_view(bsp: &Bsp) -> (Vec3, Vec3) {
    for class in STARTS {
        if let Some(entity) = bsp.entities.iter().find(|e| e.class_name() == class)
            && let Some(origin) = entity.vec3("origin")
        {
            let [pitch, yaw, _] = entity.vec3("angles").unwrap_or([0.0; 3]);
            let (pitch, yaw) = (pitch.to_radians(), yaw.to_radians());
            let forward = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), -pitch.sin()];
            // eye height of a standing player
            let height = if class == "info_observer_point" { 0.0 } else { 64.0 };
            let eye = add(origin, [0.0, 0.0, height]);
            return (eye, add(eye, scale(forward, 100.0)));
        }
    }
    let Some(world) = bsp.models.first() else { return ([0.0; 3], [100.0, 0.0, 0.0]) };
    let middle = scale(add(world.mins, world.maxs), 0.5);
    let size = sub(world.maxs, world.mins);
    let eye = add(middle, [-size[0] * 0.4, -size[1] * 0.4, size[2] * 0.6]);
    (eye, middle)
}

/// What is solid in a map: its brushes and displacements ([`Collision::new`]), and its static
/// props, by their physics models or by the boxes of their hulls. Returns too what could not be
/// read for the props: models whose physics model is broken, or of another build. The map's
/// pakfile should be mounted ([`mount_pakfile`]).
pub fn collision(vfs: &Vfs, bsp: &Bsp) -> (Collision, Vec<String>) {
    let mut collision = Collision::new(bsp);
    let mut shapes: HashMap<(u16, bool), Option<usize>> = HashMap::new();
    let mut failed = Vec::new();
    for (n, prop) in bsp.static_props.props.iter().enumerate() {
        let boxed = match prop.solid {
            solid::NONE => continue,
            solid::BBOX | solid::OBB | solid::OBB_YAW => true,
            _ => false,
        };
        let Some(name) = bsp.static_props.models.get(prop.model as usize) else { continue };
        let shape = *shapes.entry((prop.model, boxed)).or_insert_with(|| match prop_hulls(vfs, name, boxed) {
            Ok(hulls) => hulls.map(|hulls| collision.add_shape(hulls)),
            // a model the game does not have is missing from the picture as well
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                failed.push(e.to_string());
                None
            }
        });
        if let Some(shape) = shape {
            collision.add_static(n, shape, prop.origin, prop.angles);
        }
    }
    (collision, failed)
}

/// The convex hulls a model collides by: those of its physics model, or the box of its hull.
/// `None` when it has no physics model: then it does not collide.
fn prop_hulls(vfs: &Vfs, name: &str, boxed: bool) -> io::Result<Option<Vec<Hull>>> {
    let mdl = Mdl::parse(&vfs.read(name, "GAME")?).map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?;
    if boxed {
        return Ok(Some(vec![Hull::cuboid(mdl.hull.0, mdl.hull.1)]));
    }
    let Some(phy) = gmod_model::load_physics(vfs, name, mdl.checksum)? else { return Ok(None) };
    let hulls: Vec<Hull> = phy
        .solids
        .iter()
        .flat_map(|s| &s.convexes)
        .filter_map(|c| Hull::new(&c.points, &c.triangles.iter().map(|t| t.map(usize::from)).collect::<Vec<_>>()))
        .collect();
    Ok((!hulls.is_empty()).then_some(hulls))
}

/// Brush entities that are never seen: volumes for the game's logic and effects.
const UNSEEN: [&str; 12] = [
    "trigger_",
    "func_areaportal",
    "func_occluder",
    "func_viscluster",
    "func_dustmotes",
    "func_dustcloud",
    "func_smokevolume",
    "func_precipitation",
    "func_respawnroom",
    "func_regenerate",
    "func_nobuild",
    "func_clip_vphysics",
];

/// The brush models to draw with the world: the world (model 0) and every brush entity that
/// can be seen and stays where it is, with its origin and angles. Those that may move
/// ([`gmod_bsp::Entity::moves`]) are drawn where their entities are ([`brush_model`]).
pub fn placed_models(bsp: &Bsp) -> Vec<(usize, Vec3, Vec3)> {
    brush_models_to_draw(bsp, false)
}

/// The brush models to draw with the world, those that may move too if `moving`.
fn brush_models_to_draw(bsp: &Bsp, moving: bool) -> Vec<(usize, Vec3, Vec3)> {
    let mut models = vec![(0, [0.0; 3], [0.0; 3])];
    for entity in &bsp.entities {
        let Some(model) = entity.brush_model() else { continue };
        let class = entity.class_name();
        let hidden = entity.get("rendermode") == Some("10") || entity.get("StartDisabled") == Some("1");
        if model >= bsp.models.len() || hidden || !moving && entity.moves() || UNSEEN.iter().any(|unseen| class.starts_with(unseen)) {
            continue;
        }
        models.push((model, entity.vec3("origin").unwrap_or([0.0; 3]), entity.vec3("angles").unwrap_or([0.0; 3])));
    }
    models
}

/// Moves vertices from their own space into the world: turned by pitch, yaw and roll
/// (degrees), then moved to `origin`.
pub fn place(vertices: &mut [Vertex], origin: Vec3, angles: Vec3) {
    let axes = axes(angles);
    for vertex in vertices {
        vertex.position = add(origin, turn(axes, vertex.position));
        vertex.normal = turn(axes, vertex.normal);
    }
}

/// A direction of a thing's own space in the world's, by the thing's `axes`.
fn turn([forward, left, up]: [Vec3; 3], v: Vec3) -> Vec3 {
    add(add(scale(forward, v[0]), scale(left, v[1])), scale(up, v[2]))
}

/// Where a thing turned by `angles` (pitch, yaw, roll in degrees, as the game gives them) has
/// its forward, left and up.
pub fn axes(angles: Vec3) -> [Vec3; 3] {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let ((sp, cp), (sy, cy), (sr, cr)) = (pitch.sin_cos(), yaw.sin_cos(), roll.sin_cos());
    let forward = [cp * cy, cp * sy, -sp];
    let left = [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp];
    let up = [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp];
    [forward, left, up]
}

/// Where the light of models without baked light comes from, from above.
pub const SUN: Vec3 = [0.4, 0.3, 1.0];

/// Light for a model vertex without baked light: some from everywhere, more from above.
fn plain_light(normal: Vec3) -> Vec3 {
    let sun = dot(normal, normalize(SUN)).max(0.0);
    [0.35 + 0.5 * sun; 3]
}

/// Materials and textures as the world gathers them.
struct Gather<'a> {
    vfs: &'a Vfs,
    library: Library,
    textures: Vec<Texture>,
    by_name: HashMap<String, Option<usize>>,
    missing_textures: Vec<String>,
    missing_materials: Vec<String>,
}

impl Gather<'_> {
    fn new(vfs: &Vfs) -> Gather<'_> {
        Gather {
            vfs,
            library: Library::new(),
            textures: Vec::new(),
            by_name: HashMap::new(),
            missing_textures: Vec::new(),
            missing_materials: Vec::new(),
        }
    }

    fn texture(&mut self, name: &str) -> Option<usize> {
        if let Some(&known) = self.by_name.get(name) {
            return known;
        }
        let index = match self.library.texture(self.vfs, name) {
            Ok(vtf) => {
                self.textures.push(Texture { name: name.to_string(), vtf });
                Some(self.textures.len() - 1)
            }
            Err(_) => {
                self.missing_textures.push(name.to_string());
                None
            }
        };
        self.by_name.insert(name.to_string(), index);
        index
    }

    fn material(&mut self, name: &str) -> Option<Arc<Material>> {
        match self.library.material(self.vfs, name) {
            Ok(material) => Some(material),
            Err(_) => {
                if !self.missing_materials.iter().any(|m| m == name) {
                    self.missing_materials.push(name.to_string());
                }
                None
            }
        }
    }

    /// How to draw the triangles `first..first + count` with a material.
    fn surface(&mut self, name: &str, material: Option<&Material>, first: usize, count: usize) -> Surface {
        let mut s = Surface::plain(name, first, count);
        let Some(m) = material else { return s };
        s.missing = false;
        if let Some(texture) = m.texture("$basetexture") {
            s.base = self.texture(&texture);
        }
        if m.shader.starts_with("worldvertextransition")
            && let Some(texture) = m.texture("$basetexture2")
        {
            s.base2 = self.texture(&texture);
        }
        if m.flag("$alphatest") {
            s.alpha_test = Some(m.float("$alphatestreference").filter(|&r| r > 0.0).unwrap_or(0.5));
        }
        s.alpha = m.float("$alpha").unwrap_or(1.0).clamp(0.0, 1.0);
        s.additive = m.flag("$additive");
        s.translucent = m.flag("$translucent") || s.additive || s.alpha < 1.0;
        s.no_cull = m.flag("$nocull");
        s.no_fog = m.flag("$nofog");
        s.tint = m.vector("$color").unwrap_or([1.0; 3]);
        s.lit = !m.shader.starts_with("unlit");
        if m.shader == "water" {
            s.water = Some(m.vector("$fogcolor").unwrap_or([0.1, 0.2, 0.25]));
            s.translucent = true;
            s.lit = false;
        }
        s
    }
}

/// Gathers a map ready to draw. The map's pakfile should be mounted ([`mount_pakfile`]). The
/// brush entities that may move are left to be drawn where their entities are
/// ([`brush_model`]).
pub fn load(vfs: &Vfs, bsp: &Bsp) -> World {
    gather_map(vfs, bsp, false)
}

/// The map as it stands when it starts, every brush entity baked in where the map puts it:
/// for a still picture of it.
pub fn load_all(vfs: &Vfs, bsp: &Bsp) -> World {
    gather_map(vfs, bsp, true)
}

fn gather_map(vfs: &Vfs, bsp: &Bsp, moving: bool) -> World {
    let mut gather = Gather::new(vfs);
    let camera = sky::sky_camera(bsp);
    let sky_area = camera.map(|c| c.area);
    let [mut pieces, mut sky_pieces] = brushes(bsp, &mut gather, sky_area, moving);
    let missing_models = static_props(bsp, &mut gather, sky_area, [&mut pieces, &mut sky_pieces]);
    let sky_name = bsp.worldspawn().and_then(|w| w.get("skyname")).unwrap_or("").to_string();
    let sky = std::array::from_fn(|i| {
        if sky_name.is_empty() {
            return None;
        }
        let material = gather.library.material(vfs, &format!("skybox/{sky_name}{}", SKY_FACES[i])).ok()?;
        let texture = gather.texture(&material.texture("$basetexture")?)?;
        Some(SkyFace { texture, transform: material.get("$basetexturetransform").and_then(Transform::parse) })
    });
    World {
        pieces,
        textures: gather.textures,
        sky,
        skybox: camera.map(|camera| sky::Skybox { camera, pieces: sky_pieces }),
        fog: sky::fog(bsp),
        missing_materials: gather.missing_materials,
        missing_textures: gather.missing_textures,
        missing_models,
    }
}

/// The world and the brush entities, each moved to where it stands: those outside the 3D sky
/// box's area, and those in it.
fn brushes(bsp: &Bsp, gather: &mut Gather, sky_area: Option<u16>, moving: bool) -> [Vec<Piece>; 2] {
    let mut materials: HashMap<usize, Option<Arc<Material>>> = HashMap::new();
    let mut pieces = [Vec::new(), Vec::new()];
    // which faces of the world are the sky box's (the HDR faces go along with the others)
    let faces = if bsp.faces.is_empty() { &bsp.faces_hdr } else { &bsp.faces };
    let in_sky: Vec<bool> = match sky_area {
        Some(area) => faces.iter().map(|face| bsp.face_area(face) == Some(area)).collect(),
        None => Vec::new(),
    };
    // the world in two when there is a sky box; a brush entity is where the middle of its
    // bounds is, turned with it (the bounds are in its own space, around its origin)
    let mut placed = Vec::new();
    for (model, origin, angles) in brush_models_to_draw(bsp, moving) {
        if model == 0 {
            placed.push((false, model, origin, angles));
            if sky_area.is_some() {
                placed.push((true, model, origin, angles));
            }
            continue;
        }
        let middle = bsp.models.get(model).map(|m| add(origin, turn(axes(angles), scale(add(m.mins, m.maxs), 0.5))));
        placed.push((sky_area.is_some() && middle.is_some_and(|m| bsp.area_at(m) == sky_area), model, origin, angles));
    }
    for (sky, model, origin, angles) in placed {
        let options = mesh::Options { model, ..mesh::Options::default() };
        let mesh = if model == 0 {
            mesh::build_faces(bsp, options, |i| in_sky.get(i).copied().unwrap_or(false) == sky)
        } else {
            mesh::build(bsp, options)
        };
        let Some(mut piece) = brush_piece(bsp, gather, &mut materials, mesh) else { continue };
        if model != 0 {
            place(&mut piece.vertices, origin, angles);
        }
        pieces[usize::from(sky)].push(piece);
    }
    pieces
}

/// A brush model's mesh as a piece in its own space, with its light map and materials.
fn brush_piece(bsp: &Bsp, gather: &mut Gather, materials: &mut HashMap<usize, Option<Arc<Material>>>, mesh: mesh::Mesh) -> Option<Piece> {
    if mesh.indices.is_empty() {
        return None;
    }
    let vertices: Vec<Vertex> = mesh
        .vertices
        .iter()
        .map(|v| Vertex { position: v.position, normal: v.normal, uv: v.uv, lightmap_uv: v.lightmap_uv, alpha: v.alpha, light: [1.0; 3] })
        .collect();
    let mut surfaces = Vec::new();
    for batch in &mesh.batches {
        let name = bsp.materials.get(batch.material).map_or("", String::as_str);
        let material = materials.entry(batch.material).or_insert_with(|| gather.material(name)).clone();
        surfaces.push(gather.surface(name, material.as_deref(), batch.first_index as usize, batch.index_count as usize));
    }
    Some(Piece { vertices, indices: mesh.indices, lightmap: Some(mesh.lightmap), surfaces, skin: Vec::new() })
}

/// A brush model of the map on its own, to draw wherever its entity is (a door, a button, a
/// platform): one piece in the model's space with its light map, its materials and textures.
/// Empty when the map has no such model or it has nothing to draw.
pub fn brush_model(vfs: &Vfs, bsp: &Bsp, model: usize) -> World {
    let mut gather = Gather::new(vfs);
    let mut materials = HashMap::new();
    let pieces = if model == 0 || model >= bsp.models.len() {
        Vec::new()
    } else {
        let mesh = mesh::build(bsp, mesh::Options { model, ..mesh::Options::default() });
        brush_piece(bsp, &mut gather, &mut materials, mesh).into_iter().collect()
    };
    World {
        pieces,
        textures: gather.textures,
        sky: Default::default(),
        skybox: None,
        fog: None,
        missing_materials: gather.missing_materials,
        missing_textures: gather.missing_textures,
        missing_models: Vec::new(),
    }
}

/// The static props, each with its model and baked light, into the pieces of the world or of
/// the 3D sky box by their area; returns the models that could not be read.
fn static_props(bsp: &Bsp, gather: &mut Gather, sky_area: Option<u16>, [world, skybox]: [&mut Vec<Piece>; 2]) -> Vec<String> {
    let mut models: HashMap<u16, Option<Model>> = HashMap::new();
    let mut missing = Vec::new();
    let hdr = !bsp.lighting_hdr.is_empty();
    for (n, prop) in bsp.static_props.props.iter().enumerate() {
        let Some(name) = bsp.static_props.models.get(prop.model as usize) else { continue };
        let Some(model) = models
            .entry(prop.model)
            .or_insert_with(|| {
                gmod_model::load(gather.vfs, name)
                    .inspect_err(|e| missing.push(if e.kind() == io::ErrorKind::NotFound { name.clone() } else { format!("{name}: {e}") }))
                    .ok()
            })
            .as_ref()
        else {
            continue;
        };
        // the light baked for this prop: HDR when the map has it
        let files =
            if hdr { [format!("sp_hdr_{n}.vhv"), format!("sp_{n}.vhv")] } else { [format!("sp_{n}.vhv"), format!("sp_hdr_{n}.vhv")] };
        let baked = files.iter().find_map(|file| gather.vfs.read(file, "GAME").ok().and_then(|bytes| Vhv::parse(&bytes).ok()));
        let skin = usize::try_from(prop.skin).unwrap_or(0);
        let mesh = match &baked {
            Some(baked) => model.lit_mesh(0, skin, baked),
            None => model.mesh(0, skin),
        };
        let Ok(mesh) = mesh else { continue };
        let mut vertices: Vec<Vertex> = mesh
            .vertices
            .iter()
            .enumerate()
            .map(|(i, v)| Vertex {
                position: v.position,
                normal: v.normal,
                uv: v.uv,
                lightmap_uv: [0.0; 2],
                alpha: 0.0,
                light: mesh.light.as_ref().map_or([1.0; 3], |light| light[i]),
            })
            .collect();
        place(&mut vertices, prop.origin, prop.angles);
        // without baked light, light from above: in the world, so after placing
        if mesh.light.is_none() {
            for v in &mut vertices {
                v.light = plain_light(v.normal);
            }
        }
        let surfaces = model_surfaces(gather, model, &mesh);
        // in the area of the leaves the map lists for it, else of where it stands
        let first = usize::from(prop.first_leaf);
        let leaves = bsp.static_props.leaves.get(first..first + usize::from(prop.leaf_count)).unwrap_or(&[]);
        let listed = leaves.iter().filter_map(|&leaf| bsp.leaves.get(usize::from(leaf))).map(|leaf| leaf.area).find(|&area| area != 0);
        let sky = sky_area.is_some() && listed.or_else(|| bsp.area_at(prop.origin)) == sky_area;
        let piece = Piece { vertices, indices: mesh.indices, lightmap: None, surfaces, skin: Vec::new() };
        if sky { skybox.push(piece) } else { world.push(piece) }
    }
    missing
}

/// How to draw each batch of a model's triangles: the first of the materials it may have
/// that the game has.
fn model_surfaces(gather: &mut Gather, model: &Model, mesh: &gmod_model::StaticMesh) -> Vec<Surface> {
    let mut surfaces = Vec::new();
    for batch in &mesh.batches {
        let candidates = model.mdl.material_candidates(batch.texture);
        let found = candidates.iter().find_map(|candidate| gather.library.material(gather.vfs, candidate).ok().map(|m| (candidate, m)));
        let (first, count) = (batch.first_index as usize, batch.index_count as usize);
        let surface = match found {
            Some((name, material)) => gather.surface(name, Some(&material), first, count),
            None => {
                let name = candidates.first().cloned().unwrap_or_default();
                if !gather.missing_materials.contains(&name) {
                    gather.missing_materials.push(name.clone());
                }
                Surface::plain(&name, first, count)
            }
        };
        surfaces.push(surface);
    }
    surfaces
}

/// The bones of a model's vertex: those it names, their weights made to add up to 1.
fn weights(vertex: &gmod_model::Vertex) -> Weights {
    let count = usize::from(vertex.bone_count).clamp(1, 3);
    let total: f32 = vertex.weights[..count].iter().map(|w| w.max(0.0)).sum();
    let mut out = Weights { bones: vertex.bones, weights: [0.0; 3] };
    for i in 0..count {
        out.weights[i] = if total > 0.0 {
            vertex.weights[i].max(0.0) / total
        } else if i == 0 {
            1.0
        } else {
            0.0
        };
    }
    out
}

/// A model on its own, to draw wherever an entity has it: one piece in the model's space (its
/// first body group, in the pose it was made in, with the bones of every vertex to pose it),
/// with its materials and textures. The light comes from where it is drawn, so the vertices
/// have none of their own.
pub fn model(vfs: &Vfs, name: &str, skin: usize) -> io::Result<World> {
    let model = gmod_model::load(vfs, name)?;
    let mesh = model.mesh(0, skin)?;
    let mut gather = Gather::new(vfs);
    let vertices = mesh
        .vertices
        .iter()
        .map(|v| Vertex { position: v.position, normal: v.normal, uv: v.uv, lightmap_uv: [0.0; 2], alpha: 0.0, light: [1.0; 3] })
        .collect();
    let bones = mesh.vertices.iter().map(weights).collect();
    let surfaces = model_surfaces(&mut gather, &model, &mesh);
    Ok(World {
        pieces: vec![Piece { vertices, indices: mesh.indices, lightmap: None, surfaces, skin: bones }],
        textures: gather.textures,
        sky: Default::default(),
        skybox: None,
        fog: None,
        missing_materials: gather.missing_materials,
        missing_textures: gather.missing_textures,
        missing_models: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_box_faces() {
        // straight ahead along each axis lands in the middle of its face
        let axes = [[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        for (i, axis) in axes.into_iter().enumerate() {
            assert_eq!(sky_face(axis), (i, [0.5, 0.5]));
        }
        // up is the top of a side face
        let (face, uv) = sky_face(normalize([1.0, 0.0, 0.99]));
        assert_eq!(face, 0);
        assert!(uv[1] < 0.01);
    }

    #[test]
    fn texture_transforms() {
        let t = Transform::parse("center 0 0 scale 1 2 rotate 0 translate 0 0").unwrap();
        assert_eq!(t.apply([0.5, 0.25]), [0.5, 0.5]);
        let t = Transform::parse("center .5 .5 scale 1 1 rotate 90 translate 0 0").unwrap();
        let p = t.apply([1.0, 0.5]);
        assert!((p[0] - 0.5).abs() < 1e-6 && (p[1] - 1.0).abs() < 1e-6, "{p:?}");
        assert!(Transform::parse("bogus").is_none());
    }

    #[test]
    fn pieces_are_placed() {
        let mut vertices = [Vertex { position: [10.0, 0.0, 0.0], normal: [1.0, 0.0, 0.0], ..Vertex::default() }];
        // yaw 90 turns +x into +y; then the origin moves it
        place(&mut vertices, [100.0, 200.0, 300.0], [0.0, 90.0, 0.0]);
        let v = vertices[0];
        let near = |a: Vec3, b: Vec3| sub(a, b).iter().all(|d| d.abs() < 1e-4);
        assert!(near(v.position, [100.0, 210.0, 300.0]), "{:?}", v.position);
        assert!(near(v.normal, [0.0, 1.0, 0.0]), "{:?}", v.normal);
        // pitch 90 looks down: +x turns into -z
        vertices[0].normal = [1.0, 0.0, 0.0];
        place(&mut vertices, [0.0; 3], [90.0, 0.0, 0.0]);
        assert!(near(vertices[0].normal, [0.0, 0.0, -1.0]), "{:?}", vertices[0].normal);
    }

    /// A 1 x 1 BGRA texture, version 7.2.
    fn texture() -> Vec<u8> {
        let mut v = b"VTF\0".to_vec();
        for x in [7u32, 2, 80] {
            v.extend(x.to_le_bytes());
        }
        v.extend([1, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
        v.resize(52, 0);
        v.extend(12i32.to_le_bytes());
        v.push(1);
        v.extend((-1i32).to_le_bytes());
        v.resize(80, 0);
        v.extend([1, 2, 3, 4]);
        v
    }

    /// A game folder in the temporary directory, removed when dropped.
    struct Game(std::path::PathBuf);

    impl Drop for Game {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_whole_map() {
        let root = std::env::temp_dir().join(format!("gmod-world-{}", std::process::id()));
        let game = Game(root.clone());
        let files: [(&str, &[u8]); 7] = [
            ("materials/dev/dev_measuregeneric01.vmt", b"LightmappedGeneric { $basetexture dev/grid }"),
            ("materials/dev/grid.vtf", &texture()),
            ("materials/brick/brickwall001a.vmt", b"LightmappedGeneric { $basetexture brick/gone $alphatest 1 $nofog 1 }"),
            ("materials/nature/blendgroundtograss.vmt", b"WorldVertexTransition { $basetexture dev/grid $basetexture2 nature/grass }"),
            ("materials/nature/grass.vtf", &texture()),
            (
                "materials/skybox/sky_day01_01up.vmt",
                b"Sky { $basetexture dev/grid $basetexturetransform \"center 0 0 scale 1 2 rotate 0 translate 0 0\" }",
            ),
            ("maps/test.bsp", &gmod_bsp::testmap::bytes()),
        ];
        for (path, data) in files {
            let path = game.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        let mut vfs = Vfs::new();
        vfs.push(Mount::new(&["GAME"], Backing::Dir(gmod_fs::DirMount::new(&root))));
        let bsp = Bsp::parse("maps/test.bsp".into(), Arc::from(vfs.read("maps/test.bsp", "GAME").unwrap())).unwrap();
        assert!(mount_pakfile(&mut vfs, &bsp).unwrap());
        assert!(vfs.exists("materials/maps/test/patched.vmt", "GAME"));
        let world = load(&vfs, &bsp);
        vfs.remove(0);

        // the world only: floor, wall and displacement; the prop's model is not there
        assert_eq!(world.pieces.len(), 1);
        assert_eq!(world.triangles(), 2 + 2 + 32);
        let surfaces = &world.pieces[0].surfaces;
        let names: Vec<&str> = surfaces.iter().map(|s| s.material.as_str()).collect();
        assert_eq!(names, ["DEV/DEV_MEASUREGENERIC01", "BRICK/BRICKWALL001A", "NATURE/BLENDGROUNDTOGRASS"]);
        let (floor, wall, ground) = (&surfaces[0], &surfaces[1], &surfaces[2]);
        assert_eq!((floor.base, floor.missing, floor.lit), (Some(0), false, true));
        assert_eq!((wall.base, wall.alpha_test, wall.no_fog, floor.no_fog), (None, Some(0.5), true, false));
        assert_eq!((ground.base, ground.base2), (Some(0), Some(1)));
        assert_eq!(world.textures.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["dev/grid", "nature/grass"]);
        assert_eq!(world.missing_textures, ["brick/gone"]);
        assert!(world.missing_materials.is_empty(), "{:?}", world.missing_materials);
        assert_eq!(world.missing_models, ["models/props_c17/oildrum001.mdl"]);
        // the light map coordinates and the vertex light of brushes
        assert!(world.pieces[0].vertices.iter().all(|v| v.light == [1.0; 3]));
        assert!(world.pieces[0].lightmap.as_ref().is_some_and(|l| l.width > 0));
        // the sky box: only the top face is there, with its transform
        assert!(world.sky.iter().enumerate().all(|(i, face)| face.is_some() == (i == 4)));
        assert_eq!(world.sky[4].unwrap().transform.unwrap().scale, [1.0, 2.0]);
        // the player start, at eye height, looking along +y
        let (eye, target) = default_view(&bsp);
        assert_eq!(eye, [64.0, 64.0, 80.0]);
        assert_eq!(spawn_point(&bsp), Some(([64.0, 64.0, 16.0], [0.0, 90.0, 0.0])));
        assert!((target[1] - 164.0).abs() < 1e-3 && (target[0] - 64.0).abs() < 1e-3, "{target:?}");
    }

    #[test]
    fn static_props_are_solid_by_their_physics() {
        use gmod_bsp::collision::{EPSILON, mask};
        use gmod_bsp::testmap::{TestBrush, TestMap, TestStaticProp};
        use gmod_model::testmodel::{self, TestConvex};

        let root = std::env::temp_dir().join(format!("gmod-world-physics-{}", std::process::id()));
        let game = Game(root.clone());
        let hull = ([-20.0, -20.0, 0.0], [20.0, 20.0, 40.0]);
        let shape = [vec![TestConvex::cuboid([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0])]];
        let files: [(&str, Vec<u8>); 5] = [
            ("models/test/crate.mdl", testmodel::mdl(77, hull)),
            ("models/test/crate.phy", testmodel::phy(77, &shape, false, "solid { \"index\" \"0\" \"mass\" \"50\" }")),
            ("models/test/ghost.mdl", testmodel::mdl(5, hull)),
            ("models/test/other.mdl", testmodel::mdl(6, hull)),
            ("models/test/other.phy", testmodel::phy(999, &shape, false, "")),
        ];
        let prop = |model: &str, x: f32, solid: u8| TestStaticProp { model: model.into(), origin: [x, 0.0, 0.0], angles: [0.0; 3], solid };
        let map = TestMap {
            models: vec![vec![TestBrush::cuboid([-2048.0, -2048.0, -32.0], [2048.0, 2048.0, 0.0])]],
            static_props: vec![
                prop("models/test/crate.mdl", 200.0, solid::VPHYSICS),
                prop("models/test/crate.mdl", 400.0, solid::BBOX),
                prop("models/test/crate.mdl", 600.0, solid::NONE),
                prop("models/test/ghost.mdl", 800.0, solid::VPHYSICS),
                prop("models/test/other.mdl", 1000.0, solid::VPHYSICS),
                prop("models/test/gone.mdl", 1200.0, solid::VPHYSICS),
            ],
            ..TestMap::default()
        };
        for (path, data) in files {
            let path = game.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        let mut vfs = Vfs::new();
        vfs.push(Mount::new(&["GAME"], Backing::Dir(gmod_fs::DirMount::new(&root))));
        let bsp = Bsp::parse("maps/test.bsp".into(), Arc::from(map.bytes())).unwrap();
        assert_eq!(bsp.static_props.props.len(), 6);
        let (collision, failed) = collision(&vfs, &bsp);
        let down = |x: f32| collision.trace_line([x, 0.0, 100.0], [x, 0.0, -100.0], mask::SOLID);
        // by its physics model, and by the box of its hull
        assert_eq!((down(200.0).end[2], down(200.0).static_prop), (32.0 + EPSILON, Some(0)));
        assert_eq!((down(400.0).end[2], down(400.0).static_prop), (40.0 + EPSILON, Some(1)));
        // not solid, without a physics model, with another build's, and not in the game
        for x in [600.0, 800.0, 1000.0, 1200.0] {
            assert_eq!((down(x).end[2], down(x).static_prop), (EPSILON, None), "at {x}");
        }
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].contains("other.phy") && failed[0].contains("another build"), "{failed:?}");
    }

    #[test]
    fn the_weights_of_a_vertex_add_up() {
        let vertex =
            |bones: [u8; 3], weights: [f32; 3], count: u8| gmod_model::Vertex { bones, weights, bone_count: count, ..Default::default() };
        // as the model has them
        assert_eq!(weights(&vertex([3, 4, 0], [0.75, 0.25, 0.0], 2)), Weights { bones: [3, 4, 0], weights: [0.75, 0.25, 0.0] });
        // only as many as it says it has, made to add up to 1
        assert_eq!(weights(&vertex([1, 2, 9], [0.2, 0.2, 0.6], 2)).weights, [0.5, 0.5, 0.0]);
        // none at all: the first bone's
        assert_eq!(weights(&vertex([5, 0, 0], [0.0; 3], 0)), Weights { bones: [5, 0, 0], weights: [1.0, 0.0, 0.0] });
    }

    #[test]
    fn plain_light_comes_from_above() {
        assert!(plain_light([0.0, 0.0, 1.0])[0] > plain_light([0.0, 0.0, -1.0])[0]);
        assert_eq!(plain_light([0.0, 0.0, -1.0]), [0.35; 3]);
    }

    #[test]
    fn brushes_that_move_are_models_of_their_own() {
        use gmod_bsp::testmap::{TestBrush, TestMap};
        let entities =
            "{\n\"classname\" \"func_door\"\n\"model\" \"*1\"\n}\n{\n\"classname\" \"func_wall\"\n\"model\" \"*2\"\n}\n".to_string();
        let block = |x: f32| vec![TestBrush::cuboid([x, -32.0, 0.0], [x + 8.0, 32.0, 64.0])];
        let models = vec![vec![TestBrush::cuboid([-512.0, -512.0, -32.0], [1024.0, 512.0, 0.0])], block(100.0), block(200.0)];
        // the door's face toward -x, in its own space
        let face = gmod_bsp::testmap::TestFace {
            corners: vec![[100.0, -32.0, 0.0], [100.0, 32.0, 0.0], [100.0, 32.0, 64.0], [100.0, -32.0, 64.0]],
            material: "TEST/DOOR",
            flags: 0,
        };
        let test = TestMap { models, entities, model_faces: vec![(1, face)], ..TestMap::default() };
        let bsp = Bsp::parse("t.bsp".into(), Arc::from(test.bytes())).unwrap();
        // the door is not baked into the map; the wall is
        let placed: Vec<usize> = placed_models(&bsp).into_iter().map(|(m, _, _)| m).collect();
        assert_eq!(placed, [0, 2]);
        let vfs = Vfs::new();
        let door = brush_model(&vfs, &bsp, 1);
        assert_eq!(door.pieces.len(), 1);
        let piece = &door.pieces[0];
        assert!(piece.lightmap.is_some() && !piece.surfaces.is_empty());
        assert!(piece.vertices.iter().all(|v| (100.0..=108.0).contains(&v.position[0])), "in its own space");
        assert!(brush_model(&vfs, &bsp, 0).pieces.is_empty() && brush_model(&vfs, &bsp, 9).pieces.is_empty());
        // a still picture has the door where the map puts it
        assert_eq!(load_all(&vfs, &bsp).pieces.len(), load(&vfs, &bsp).pieces.len() + 1);
    }
}
