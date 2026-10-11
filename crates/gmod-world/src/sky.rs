//! The 3D sky box and fog, as the map's entities set them up.
//!
//! The 3D sky box is a sealed room of the map, a small model of what lies far around the
//! playable world, with a `sky_camera` standing in it. It is drawn behind the world, seen
//! wherever the world leaves the view open (its sky faces are not drawn): from the camera's
//! origin plus the eye's place divided by the camera's `scale` (16 unless it says), looking the
//! eye's way, so that each unit of the room looks `scale` units big. What belongs to it is what
//! is in the area of the camera's leaf - compilers flood areas through open space, so a sealed
//! room is an area of its own: the world's faces seen from there, the brush entities and the
//! static props standing there. The world is drawn without them.
//!
//! Fog is linear with the distance from the eye: none up to `start`, then more and more of its
//! colour up to `end`, where it covers `max_density` of what is behind it. The world's is the
//! `env_fog_controller`'s (the master one when there are several), the sky box's the sky
//! camera's own keys. The sky box itself is not fogged.

use gmod_bsp::{Bsp, Entity};

use crate::{Vec3, add, scale};

/// The fog of the world or of the sky box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fog {
    /// Linear.
    pub color: Vec3,
    /// Where it starts and where it is thickest, in units from the eye.
    pub start: f32,
    pub end: f32,
    /// How much of what is behind it it covers at most (0 to 1).
    pub max_density: f32,
}

impl Fog {
    /// The fog of an entity with the keys of `env_fog_controller` (`fogenable`, `fogcolor`,
    /// `fogstart`, `fogend`, `fogmaxdensity`), as Hammer fills them in when they are left out;
    /// none when it is not enabled.
    pub fn from_entity(entity: &Entity) -> Option<Fog> {
        let number = |key: &str, default: f32| {
            entity.get(key).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(default)
        };
        if number("fogenable", 0.0) == 0.0 {
            return None;
        }
        Some(Fog {
            color: entity.get("fogcolor").and_then(color).unwrap_or([1.0; 3]),
            start: number("fogstart", 500.0),
            end: number("fogend", 2000.0),
            max_density: number("fogmaxdensity", 1.0).clamp(0.0, 1.0),
        })
    }

    /// How much of the fog's colour covers what is `distance` away.
    pub fn amount(&self, distance: f32) -> f32 {
        ((distance - self.start) * self.inverse_range()).clamp(0.0, 1.0) * self.max_density
    }

    /// One over the distance from `start` to `end`; where they meet, the fog is all there at
    /// once.
    pub fn inverse_range(&self) -> f32 {
        1.0 / (self.end - self.start).max(1e-3)
    }
}

/// An sRGB colour of 0-255 numbers (`"R G B"`), made linear.
fn color(text: &str) -> Option<Vec3> {
    let mut values = text.split_whitespace().map(|v| v.parse::<f32>().ok().filter(|v| v.is_finite()));
    let mut next = || -> Option<f32> {
        let c = (values.next()?? / 255.0).clamp(0.0, 1.0);
        Some(if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) })
    };
    Some([next()?, next()?, next()?])
}

/// The world's fog: the master `env_fog_controller`'s, else the first one's.
pub fn fog(bsp: &Bsp) -> Option<Fog> {
    let controllers = || bsp.entities.iter().filter(|e| e.class_name() == "env_fog_controller");
    let master = |e: &&Entity| e.get("spawnflags").and_then(|f| f.trim().parse::<u32>().ok()).is_some_and(|f| f & 1 != 0);
    controllers().find(master).or_else(|| controllers().next()).and_then(Fog::from_entity)
}

/// The camera of the 3D sky box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyCamera {
    pub origin: Vec3,
    /// How many units of the world a unit of the sky box looks: 16 unless the map says.
    pub scale: f32,
    /// The area of the leaf it stands in: what is there is the sky box.
    pub area: u16,
    /// The sky box's own fog. Its distances are taken in the sky box's units, from the
    /// camera's eye in the sky box (as drawing the room from there measures them), not in the
    /// world's units.
    pub fog: Option<Fog>,
}

impl SkyCamera {
    /// Where the sky box is seen from, for an eye at `eye` in the world.
    pub fn eye(&self, eye: Vec3) -> Vec3 {
        add(self.origin, scale(eye, 1.0 / self.scale))
    }
}

/// The map's `sky_camera`: none when it has none, or when it stands where nothing is (in the
/// solid or outside the world) or in the area players start in - a 3D sky box is a room of its
/// own.
pub fn sky_camera(bsp: &Bsp) -> Option<SkyCamera> {
    let entity = bsp.entities.iter().find(|e| e.class_name() == "sky_camera")?;
    let origin = entity.vec3("origin")?;
    let area = bsp.area_at(origin)?;
    // an eye above the feet: the feet stand on the floor
    if let Some((start, _)) = crate::spawn_point(bsp)
        && bsp.area_at(add(start, [0.0, 0.0, 16.0])) == Some(area)
    {
        return None;
    }
    let scale = entity.get("scale").and_then(|s| s.trim().parse::<f32>().ok()).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(16.0);
    Some(SkyCamera { origin, scale, area, fog: Fog::from_entity(entity) })
}

/// The 3D sky box ready to draw: its camera and what is in it.
#[derive(Debug, Clone)]
pub struct Skybox {
    pub camera: SkyCamera,
    /// Its faces, brush entities and static props, as [`crate::World::pieces`]; their
    /// textures are the world's.
    pub pieces: Vec<crate::Piece>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gmod_bsp::testmap::{TestBrush, TestFace, TestMap};
    use gmod_fs::Vfs;

    use super::*;

    #[test]
    fn fog_by_distance() {
        let fog = Fog { color: [0.0; 3], start: 100.0, end: 300.0, max_density: 0.5 };
        assert_eq!(fog.amount(50.0), 0.0);
        assert_eq!(fog.amount(200.0), 0.25);
        assert_eq!(fog.amount(1000.0), 0.5);
        // where start and end meet it is there at once
        let wall = Fog { end: 100.0, ..fog };
        assert_eq!((wall.amount(99.0), wall.amount(101.0)), (0.0, 0.5));
        // as the shader gets it
        assert_eq!(crate::gpu::fog_params(Some(&fog)), [0.0, 0.0, 0.0, 0.5, 100.0, 0.005, 1.0, 0.0]);
        assert_eq!(crate::gpu::fog_params(None), [0.0; 8]);
    }

    #[test]
    fn fog_keys() {
        let entity = |pairs: &[(&str, &str)]| Entity { pairs: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        let fog = Fog::from_entity(&entity(&[("fogenable", "1"), ("fogcolor", "255 0 128"), ("fogstart", "64"), ("fogmaxdensity", "2")]));
        let fog = fog.unwrap();
        assert_eq!((fog.color[0], fog.color[1]), (1.0, 0.0));
        assert!((fog.color[2] - 0.2158).abs() < 1e-3, "{:?}", fog.color);
        // left out: Hammer's defaults; too thick: all of it
        assert_eq!((fog.start, fog.end, fog.max_density), (64.0, 2000.0, 1.0));
        assert_eq!(Fog::from_entity(&entity(&[("fogenable", "0"), ("fogstart", "64")])), None);
        assert_eq!(Fog::from_entity(&entity(&[("fogcolor", "1 2 3")])), None);
        assert_eq!(Fog::from_entity(&entity(&[("fogenable", "1"), ("fogcolor", "bad")])).unwrap().color, [1.0; 3]);
    }

    /// A room of the world from (0, 0, 0) to (512, 512, 256), its ceiling sky; a sealed sky box
    /// from (2048, 0, 0) to (2112, 64, 64) with a floor and a sky camera; fog.
    fn map() -> TestMap {
        let walls = |lo: Vec3, hi: Vec3| {
            let t = 16.0;
            vec![
                TestBrush::cuboid([lo[0] - t, lo[1] - t, lo[2] - t], [hi[0] + t, hi[1] + t, lo[2]]),
                TestBrush::cuboid([lo[0] - t, lo[1] - t, hi[2]], [hi[0] + t, hi[1] + t, hi[2] + t]),
                TestBrush::cuboid([lo[0] - t, lo[1] - t, lo[2]], [lo[0], hi[1] + t, hi[2]]),
                TestBrush::cuboid([hi[0], lo[1] - t, lo[2]], [hi[0] + t, hi[1] + t, hi[2]]),
                TestBrush::cuboid([lo[0], lo[1] - t, lo[2]], [hi[0], lo[1], hi[2]]),
                TestBrush::cuboid([lo[0], hi[1], lo[2]], [hi[0], hi[1] + t, hi[2]]),
            ]
        };
        let mut world = walls([0.0; 3], [512.0, 512.0, 256.0]);
        world.extend(walls([2048.0, 0.0, 0.0], [2112.0, 64.0, 64.0]));
        // floors, facing up; the room's ceiling, facing down, is sky
        let floor = |x: f32, side: f32, material| TestFace {
            corners: vec![[x, 0.0, 0.0], [x, side, 0.0], [x + side, side, 0.0], [x + side, 0.0, 0.0]],
            material,
            flags: 0,
        };
        let ceiling = TestFace {
            corners: vec![[0.0, 0.0, 256.0], [512.0, 0.0, 256.0], [512.0, 512.0, 256.0], [0.0, 512.0, 256.0]],
            material: "TOOLS/TOOLSSKYBOX",
            flags: gmod_bsp::surf::SKY,
        };
        TestMap {
            models: vec![world],
            faces: vec![floor(0.0, 512.0, "ROOM/FLOOR"), ceiling, floor(2048.0, 64.0, "SKY/FLOOR")],
            areas: vec![([0.0; 3], [512.0, 512.0, 256.0], 1), ([2048.0, 0.0, 0.0], [2112.0, 64.0, 64.0], 2)],
            entities: concat!(
                "{\n\"classname\" \"info_player_start\"\n\"origin\" \"256 256 0\"\n}\n",
                "{\n\"classname\" \"sky_camera\"\n\"origin\" \"2080 32 32\"\n\"scale\" \"16\"\n\"fogenable\" \"1\"\n",
                "\"fogcolor\" \"0 255 0\"\n\"fogstart\" \"10\"\n\"fogend\" \"40\"\n}\n",
                "{\n\"classname\" \"env_fog_controller\"\n\"fogenable\" \"1\"\n\"fogcolor\" \"0 0 255\"\n",
                "\"fogstart\" \"100\"\n\"fogend\" \"400\"\n\"fogmaxdensity\" \"0.75\"\n}\n",
            )
            .to_string(),
            ..TestMap::default()
        }
    }

    #[test]
    fn the_sky_box_is_split_from_the_world() {
        let bsp = gmod_bsp::Bsp::parse("maps/test.bsp".into(), Arc::from(map().bytes())).unwrap();
        // the rooms are areas of their own; the solid and the void are none
        assert_eq!(bsp.area_at([256.0, 256.0, 128.0]), Some(1));
        assert_eq!(bsp.area_at([2080.0, 32.0, 32.0]), Some(2));
        assert_eq!(bsp.area_at([1024.0, 32.0, 32.0]), None);
        let areas: Vec<_> = bsp.faces.iter().map(|f| bsp.face_area(f)).collect();
        assert_eq!(areas, [Some(1), Some(1), Some(2)]);

        let camera = sky_camera(&bsp).unwrap();
        assert_eq!((camera.origin, camera.scale, camera.area), ([2080.0, 32.0, 32.0], 16.0, 2));
        let sky_fog = camera.fog.unwrap();
        assert_eq!((sky_fog.color, sky_fog.start, sky_fog.end, sky_fog.max_density), ([0.0, 1.0, 0.0], 10.0, 40.0, 1.0));
        // an eye in the world is seen in the sky box a sixteenth as far from the camera
        assert_eq!(camera.eye([160.0, -320.0, 64.0]), [2090.0, 12.0, 36.0]);
        let fog = fog(&bsp).unwrap();
        assert_eq!((fog.color, fog.start, fog.end, fog.max_density), ([0.0, 0.0, 1.0], 100.0, 400.0, 0.75));

        // the world without the sky box's floor, the sky box with it alone; sky faces in neither
        let world = crate::load(&Vfs::new(), &bsp);
        let materials =
            |pieces: &[crate::Piece]| -> Vec<String> { pieces.iter().flat_map(|p| &p.surfaces).map(|s| s.material.clone()).collect() };
        assert_eq!(materials(&world.pieces), ["ROOM/FLOOR"]);
        let skybox = world.skybox.as_ref().unwrap();
        assert_eq!(skybox.camera, camera);
        assert_eq!(materials(&skybox.pieces), ["SKY/FLOOR"]);
        assert!(skybox.pieces[0].vertices.iter().all(|v| v.position[0] >= 2048.0));
        assert_eq!(world.fog, Some(fog));
        assert_eq!(world.triangles(), 4);
    }

    #[test]
    fn static_props_go_where_they_stand() {
        use gmod_bsp::testmap::TestStaticProp;
        use gmod_fs::{Backing, DirMount, Mount};
        use gmod_model::testmodel;

        let root = std::env::temp_dir().join(format!("gmod-world-skybox-{}", std::process::id()));
        let stem = root.join("models/test/rock");
        std::fs::create_dir_all(stem.parent().unwrap()).unwrap();
        std::fs::write(stem.with_extension("mdl"), testmodel::mdl(9, ([-8.0; 3], [8.0; 3]))).unwrap();
        std::fs::write(stem.with_extension("vvd"), testmodel::empty_vvd(9)).unwrap();
        std::fs::write(stem.with_extension("dx90.vtx"), testmodel::empty_vtx(9)).unwrap();
        let mut vfs = Vfs::new();
        vfs.push(Mount::new(&["GAME"], Backing::Dir(DirMount::new(&root))));
        let prop = |origin| TestStaticProp { model: "models/test/rock.mdl".into(), origin, angles: [0.0; 3], solid: 0 };
        let test = TestMap { static_props: vec![prop([100.0, 100.0, 8.0]), prop([2070.0, 20.0, 8.0])], ..map() };
        let bsp = gmod_bsp::Bsp::parse("maps/test.bsp".into(), Arc::from(test.bytes())).unwrap();
        let world = crate::load(&vfs, &bsp);
        let _ = std::fs::remove_dir_all(&root);
        // the floor and a prop each
        assert_eq!(world.missing_models, Vec::<String>::new());
        assert_eq!((world.pieces.len(), world.skybox.unwrap().pieces.len()), (2, 2));
    }

    #[test]
    fn no_sky_box_where_the_players_are() {
        // a camera in the room players start in, or in the void, makes no sky box
        let mut test = map();
        test.entities = test.entities.replace("\"2080 32 32\"", "\"128 128 32\"");
        let bsp = gmod_bsp::Bsp::parse("maps/test.bsp".into(), Arc::from(test.bytes())).unwrap();
        assert_eq!(sky_camera(&bsp), None);
        let world = crate::load(&Vfs::new(), &bsp);
        assert!(world.skybox.is_none());
        assert_eq!(world.pieces.iter().flat_map(|p| &p.surfaces).count(), 2);
        test.entities = test.entities.replace("\"128 128 32\"", "\"1024 32 32\"");
        let bsp = gmod_bsp::Bsp::parse("maps/test.bsp".into(), Arc::from(test.bytes())).unwrap();
        assert_eq!(sky_camera(&bsp), None);
    }
}
