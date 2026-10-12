//! Materials that change with time, and overlays.

use gmod_bsp::testmap::{TestFace, TestMap, TestOverlay};
use gmod_material::{Proxies, Vmt, Vtf, vmt};
use gmod_world::{Piece, Surface, Vertex};

use super::*;

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

fn green(p: &[u8]) -> bool {
    p[1] > 200 && p[0] < 40 && p[2] < 40
}

/// A VTF 7.2 of RGBA8888 with one mipmap, `side` texels a side: a frame for each list of texels.
fn vtf_bytes(side: u16, frames: &[Vec<[u8; 4]>]) -> Vec<u8> {
    let mut v = b"VTF\0".to_vec();
    for x in [7u32, 2, 80] {
        v.extend(x.to_le_bytes());
    }
    v.extend(side.to_le_bytes());
    v.extend(side.to_le_bytes());
    v.extend(0u32.to_le_bytes()); // flags: it repeats
    v.extend((frames.len() as u16).to_le_bytes());
    v.resize(52, 0);
    v.extend(0i32.to_le_bytes()); // RGBA8888
    v.push(1);
    v.extend((-1i32).to_le_bytes()); // no thumbnail
    v.resize(80, 0);
    v.extend(frames.iter().flatten().flatten());
    v
}

/// A square 64 units a side facing up, unlit, of a texture with these frames and a material
/// with `proxies`.
fn square(proxies: &str, frames: &[Vec<[u8; 4]>]) -> World {
    let text = format!("UnlitGeneric {{ $basetexture square Proxies {{ {proxies} }} }}");
    let Vmt::Material(material) = vmt::parse(&text).unwrap() else { unreachable!() };
    let vertices = [[0.0, 0.0], [0.0, 64.0], [64.0, 64.0], [64.0, 0.0]]
        .map(|[x, y]| Vertex { position: [x, y, 0.0], normal: [0.0, 0.0, 1.0], uv: [x / 64.0, y / 64.0], ..Default::default() })
        .to_vec();
    let surface = Surface {
        material: "square".to_string(),
        first: 0,
        count: 6,
        base: Some(0),
        base2: None,
        alpha_test: None,
        translucent: false,
        additive: false,
        no_fog: true,
        lit: false,
        no_cull: false,
        tint: [1.0; 3],
        alpha: 1.0,
        water: None,
        missing: false,
        transform: None,
        frame: 0,
        proxies: Proxies::new(&material).map(Arc::new),
        modulate: false,
        overlay: false,
    };
    let vtf = Vtf::parse(vtf_bytes(4, frames)).unwrap();
    World {
        pieces: vec![Piece { vertices, indices: vec![0, 1, 2, 0, 2, 3], lightmap: None, surfaces: vec![surface], skin: Vec::new() }],
        textures: vec![gmod_world::Texture { name: "square".to_string(), vtf: Arc::new(vtf) }],
        sky: Default::default(),
        skybox: None,
        fog: None,
        missing_materials: Vec::new(),
        missing_textures: Vec::new(),
        missing_models: Vec::new(),
    }
}

/// The square seen from above, at `time` seconds.
fn at_time(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut Renderer, time: f32) -> Vec<u8> {
    renderer.set_time(time);
    picture(device, queue, renderer, &Camera::looking_at([32.0, 32.0, 100.0], [32.0, 32.1, 0.0], 60.0))
}

#[test]
fn scrolling_textures_move() {
    let Some((device, queue)) = device_or_skip() else { return };
    // the left half of the texture red, the right half green, scrolling half of it a second
    let texels: Vec<[u8; 4]> = (0..16).map(|i| if i % 4 < 2 { RED } else { GREEN }).collect();
    let world = square(r#"TextureScroll { texturescrollvar $basetexturetransform texturescrollrate 0.5 }"#, &[texels]);
    let mut renderer = Renderer::new(&device, &queue, FORMAT, &world);
    let before = at_time(&device, &queue, &mut renderer, 0.0);
    let after = at_time(&device, &queue, &mut renderer, 1.0);
    let pixels = || (0..64 * 64).map(|i| (at(&before, i % 64, i / 64), at(&after, i % 64, i / 64)));
    let turned = pixels().filter(|(a, b)| red(a) && green(b)).count();
    let back = pixels().filter(|(a, b)| green(a) && red(b)).count();
    assert!(turned > 50 && back > 50, "{turned} red pixels turned green, {back} the other way");
    assert_eq!(pixels().filter(|(a, b)| red(a) && red(b)).count(), 0, "the texture did not move");
    // a whole repeat later it is back
    let again = at_time(&device, &queue, &mut renderer, 2.0);
    assert_eq!((0..64 * 64).filter(|i| red(at(&before, i % 64, i / 64)) && !red(at(&again, i % 64, i / 64))).count(), 0);
}

#[test]
fn animated_textures_show_their_frames() {
    let Some((device, queue)) = device_or_skip() else { return };
    let frames = [vec![RED; 16], vec![GREEN; 16]];
    let world = square(
        r#"AnimatedTexture { animatedtexturevar $basetexture animatedtextureframenumvar $frame animatedtextureframerate 1 }"#,
        &frames,
    );
    let mut renderer = Renderer::new(&device, &queue, FORMAT, &world);
    let middle = |pixels: &[u8], is: fn(&[u8]) -> bool| (24..40).all(|y| (24..40).all(|x| is(at(pixels, x, y))));
    assert!(middle(&at_time(&device, &queue, &mut renderer, 0.0), red), "frame 0");
    assert!(middle(&at_time(&device, &queue, &mut renderer, 1.2), green), "frame 1");
    assert!(middle(&at_time(&device, &queue, &mut renderer, 2.5), red), "frame 2 is frame 0 again");
}

/// A game folder in the temporary directory, removed when dropped.
struct Game(std::path::PathBuf);

impl Drop for Game {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A game folder named after `name` with `files`, mounted.
fn game(name: &str, files: &[(&str, Vec<u8>)]) -> (Game, gmod_fs::Vfs) {
    let root = std::env::temp_dir().join(format!("gmod-render-{name}-{}", std::process::id()));
    let game = Game(root.clone());
    for (path, data) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    let mut vfs = gmod_fs::Vfs::new();
    vfs.push(gmod_fs::Mount::new(&["GAME"], gmod_fs::Backing::Dir(gmod_fs::DirMount::new(&root))));
    (game, vfs)
}

#[test]
fn overlays_show_on_walls() {
    let Some((device, queue)) = device_or_skip() else { return };
    let files: [(&str, Vec<u8>); 6] = [
        ("materials/brick/wall.vmt", b"LightmappedGeneric { $basetexture brick/red }".to_vec()),
        ("materials/brick/red.vtf", vtf_bytes(4, &[vec![RED; 16]])),
        ("materials/decals/sign.vmt", b"LightmappedGeneric { $basetexture decals/green $translucent 1 $decal 1 }".to_vec()),
        ("materials/decals/green.vtf", vtf_bytes(4, &[vec![GREEN; 16]])),
        ("materials/decals/crack.vmt", b"DecalModulate { $basetexture decals/dark }".to_vec()),
        ("materials/decals/dark.vtf", vtf_bytes(4, &[vec![[64, 64, 64, 255]; 16]])),
    ];
    let (_game, vfs) = game("overlays", &files);

    // a wall at x = 128 facing -x, filling half the picture across; a green sign in its
    // middle, a dark crack higher up
    let corners = vec![[128.0, 128.0, 128.0], [128.0, 0.0, 128.0], [128.0, 0.0, 0.0], [128.0, 128.0, 0.0]];
    let on_wall = |material, origin, size| TestOverlay::square(material, vec![0], origin, [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], size);
    let map = TestMap {
        faces: vec![TestFace { corners, material: "BRICK/WALL", flags: 0 }],
        overlays: vec![on_wall("DECALS/SIGN", [128.0, 64.0, 64.0], 32.0), on_wall("DECALS/CRACK", [128.0, 64.0, 104.0], 16.0)],
        ..TestMap::default()
    };
    let bsp = gmod_bsp::Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    let world = gmod_world::load(&vfs, &bsp);
    assert!(world.missing_materials.is_empty() && world.missing_textures.is_empty());

    let pixels = draw(&device, &queue, &world, &Camera::looking_at([0.0, 64.0, 64.0], [128.0, 64.0, 64.0], 90.0));
    // the sign in the middle, the wall around it
    assert!((28..36).all(|y| (28..36).all(|x| green(at(&pixels, x, y)))), "{:?}", at(&pixels, 32, 32));
    assert!([(32, 44), (20, 32), (44, 32)].iter().all(|&(x, y)| red(at(&pixels, x, y))));
    // the crack: the red behind it at half, as twice its grey (a quarter) has it
    let darker = |p: &[u8]| (90..170).contains(&p[0]) && p[1] < 30 && p[2] < 30;
    let cracked = (0..64 * 64).filter(|i| darker(at(&pixels, i % 64, i / 64))).count();
    assert!(cracked > 10, "{cracked} darker pixels");
}

#[test]
fn overlays_keep_their_order_wherever_the_eye_is() {
    let Some((device, queue)) = device_or_skip() else { return };
    let files: [(&str, Vec<u8>); 6] = [
        ("materials/floor/red.vmt", b"LightmappedGeneric { $basetexture floor/red }".to_vec()),
        ("materials/floor/red.vtf", vtf_bytes(4, &[vec![RED; 16]])),
        ("materials/decals/under.vmt", b"LightmappedGeneric { $basetexture decals/green $translucent 1 }".to_vec()),
        ("materials/decals/green.vtf", vtf_bytes(4, &[vec![GREEN; 16]])),
        ("materials/decals/over.vmt", b"LightmappedGeneric { $basetexture decals/blue $translucent 1 }".to_vec()),
        ("materials/decals/blue.vtf", vtf_bytes(4, &[vec![[0, 0, 255, 255]; 16]])),
    ];
    let (_game, vfs) = game("overlay-order", &files);
    // a long floor; two overlays overlapping from x = 200 to 232, the one of the higher
    // render order first in the lump
    let corners = vec![[0.0, -64.0, 0.0], [0.0, 64.0, 0.0], [512.0, 64.0, 0.0], [512.0, -64.0, 0.0]];
    let on_floor = |material, x| TestOverlay::square(material, vec![0], [x, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 64.0);
    let mut over = on_floor("DECALS/OVER", 232.0);
    over.overlay.render_order = 1;
    let map = TestMap {
        faces: vec![TestFace { corners, material: "FLOOR/RED", flags: 0 }],
        overlays: vec![over, on_floor("DECALS/UNDER", 200.0)],
        ..TestMap::default()
    };
    let bsp = gmod_bsp::Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    let world = gmod_world::load(&vfs, &bsp);
    assert!(world.missing_materials.is_empty() && world.missing_textures.is_empty());

    // from either end, either one the nearer: the higher order on top
    let blue = |p: &[u8]| p[2] > 200 && p[0] < 40 && p[1] < 40;
    let mut renderer = Renderer::new(&device, &queue, FORMAT, &world);
    for eye in [[100.0, 0.0, 80.0], [332.0, 0.0, 80.0]] {
        let pixels = picture(&device, &queue, &mut renderer, &Camera::looking_at(eye, [216.0, 0.0, 0.0], 60.0));
        assert!((30..34).all(|y| (30..34).all(|x| blue(at(&pixels, x, y)))), "from {eye:?}: {:?}", at(&pixels, 32, 32));
    }
}
