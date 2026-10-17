//! The 3D sky box behind the world, and fog.

use gmod_world::sky::{Fog, SkyCamera, Skybox};
use gmod_world::{Piece, Surface, Vertex};

use super::*;

/// A wall facing -x at `x`, from `y[0]` to `y[1]` and `z[0]` to `z[1]`, of one colour: the
/// purple of the missing texture (it is sampled in one spot) times `light`.
fn wall(x: f32, y: [f32; 2], z: [f32; 2], light: [f32; 3]) -> Piece {
    // clockwise seen from the front, from -x, where +y is to the left
    let vertices = [[y[1], z[1]], [y[0], z[1]], [y[0], z[0]], [y[1], z[0]]]
        .map(|[y, z]| Vertex { position: [x, y, z], normal: [-1.0, 0.0, 0.0], uv: [0.06; 2], light, ..Default::default() })
        .to_vec();
    let surface = Surface {
        material: "wall".to_string(),
        first: 0,
        count: 6,
        base: None,
        base2: None,
        detail: None,
        bump: None,
        alpha_test: None,
        translucent: false,
        additive: false,
        no_fog: false,
        lit: true,
        no_cull: false,
        tint: [1.0; 3],
        alpha: 1.0,
        water: None,
        refraction: None,
        missing: true,
        transform: None,
        frame: 0,
        proxies: None,
        modulate: false,
        overlay: false,
    };
    Piece { vertices, indices: vec![0, 1, 2, 0, 2, 3], lightmap: None, surfaces: vec![surface], skin: Vec::new() }
}

fn world(pieces: Vec<Piece>, skybox: Option<Skybox>, fog: Option<Fog>) -> World {
    World {
        pieces,
        textures: Vec::new(),
        sky: Default::default(),
        skybox,
        fog,
        missing_materials: Vec::new(),
        missing_textures: Vec::new(),
        missing_models: Vec::new(),
    }
}

const BLUE: [f32; 3] = [0.0, 0.0, 1.0];
const RED: [f32; 3] = [1.0, 0.0, 0.0];
const GREEN: [f32; 3] = [0.0, 1.0, 0.0];

fn blue(p: &[u8]) -> bool {
    p[2] > 200 && p[0] < 40 && p[1] < 40
}

fn green(p: &[u8]) -> bool {
    p[1] > 200 && p[0] < 40 && p[2] < 40
}

/// Whether all pixels of columns `x` and rows `y` are what `is` says.
fn all(pixels: &[u8], x: std::ops::Range<usize>, y: std::ops::Range<usize>, is: impl Fn(&[u8]) -> bool) -> bool {
    y.flat_map(|y| x.clone().map(move |x| (x, y))).all(|(x, y)| is(at(pixels, x, y)))
}

#[test]
fn draws_the_3d_sky_box_behind_the_world() {
    let Some((device, queue)) = device_or_skip() else { return };
    // the eye looks along +x; the sky box is seen from 1/16 of its place, from the sky camera
    let eye = [1600.0, -320.0, 64.0];
    let camera = SkyCamera {
        origin: [-1000.0, 500.0, 200.0],
        scale: 16.0,
        area: 1,
        // in the world's units: what is 32 units of the sky box away is 512 of the world's
        fog: Some(Fog { color: GREEN, start: 400.0, end: 500.0, max_density: 0.5 }),
    };
    let sky_eye = camera.eye(eye);
    assert_eq!(sky_eye, [-900.0, 480.0, 204.0]);
    // in the sky box, 32 units ahead: a red wall from 8 to 64 units to the right, which its
    // fog half covers (counted in the sky box's units, it would not have started), and one to
    // the left
    let ahead = sky_eye[0] + 32.0;
    let z = [sky_eye[2] - 64.0, sky_eye[2] + 64.0];
    let skybox = Skybox {
        camera,
        pieces: vec![
            wall(ahead, [sky_eye[1] - 64.0, sky_eye[1] - 8.0], z, RED),
            wall(ahead, [sky_eye[1] + 8.0, sky_eye[1] + 64.0], z, RED),
        ],
    };
    // the world: a blue wall far ahead on the left, farther than the sky box was drawn
    let pieces = vec![wall(eye[0] + 3000.0, [eye[1], eye[1] + 5000.0], [-5000.0, 5000.0], BLUE)];
    let world = world(pieces, Some(skybox), None);
    let pixels = draw(&device, &queue, &world, &Camera { eye, yaw: 0.0, pitch: 0.0, fov: 90.0 });

    // the world on the left, over the sky box's wall there, and not fogged by the sky box's fog
    assert!(all(&pixels, 0..31, 0..64, blue), "{:?}", at(&pixels, 16, 32));
    // between the middle and the sky box's wall: open, the clear colour
    let clear = |p: &[u8]| p[2] > p[0] && p[0] > 100 && p[1] > 100;
    assert!(all(&pixels, 33..42, 0..64, clear), "{:?}", at(&pixels, 37, 32));
    // the wall 8 units of 32 to the right, where a field of view of 90 puts it: a third of the
    // way to the edge; red half covered by green fog
    let fogged_red = |p: &[u8]| p[0] > 150 && p[1] > 150 && p[2] < 40;
    assert!(all(&pixels, 44..64, 0..64, fogged_red), "{:?}", at(&pixels, 50, 32));

    // without a sky box, the clear colour on the right
    let world = World { skybox: None, ..world };
    let pixels = draw(&device, &queue, &world, &Camera { eye, yaw: 0.0, pitch: 0.0, fov: 90.0 });
    assert!(all(&pixels, 33..64, 0..64, clear));
}

#[test]
fn fog_by_the_distance_from_the_eye() {
    let Some((device, queue)) = device_or_skip() else { return };
    let eye = [0.0, 0.0, 64.0];
    let fog = Fog { color: GREEN, start: 500.0, end: 1000.0, max_density: 1.0 };
    // near on the left, far below on the right
    let pieces = vec![wall(200.0, [0.0, 1000.0], [-1000.0, 1000.0], BLUE), wall(3000.0, [-3000.0, 0.0], [-3000.0, eye[2]], BLUE)];
    let mut renderer = Renderer::new(&device, &queue, FORMAT, &world(pieces, None, Some(fog)));
    // a model far above on the right, black (no light where it is), turned to face the eye
    // and made 20 times as big: fogged as far as it is in the world
    let model = renderer.add_model(&device, &queue, &square(None));
    renderer.set_entities(vec![Placed {
        model,
        origin: [2900.0, -1280.0, eye[2]],
        angles: [-90.0, 0.0, 0.0],
        scale: 20.0,
        ..Placed::default()
    }]);
    let pixels = picture(&device, &queue, &mut renderer, &Camera { eye, yaw: 0.0, pitch: 0.0, fov: 90.0 });

    assert!(all(&pixels, 4..28, 4..60, blue), "the near wall: {:?}", at(&pixels, 16, 32));
    assert!(all(&pixels, 36..60, 40..60, green), "the far wall: {:?}", at(&pixels, 48, 48));
    assert!(all(&pixels, 36..48, 16..28, green), "the far model: {:?}", at(&pixels, 42, 22));

    // a surface of `$nofog` is shown as it is, however far
    let mut far = wall(3000.0, [-3000.0, 0.0], [-3000.0, eye[2]], BLUE);
    far.surfaces[0].no_fog = true;
    let pixels = draw(&device, &queue, &world(vec![far], None, Some(fog)), &Camera { eye, yaw: 0.0, pitch: 0.0, fov: 90.0 });
    assert!(all(&pixels, 36..60, 40..60, blue), "the far wall of $nofog: {:?}", at(&pixels, 48, 48));
}
