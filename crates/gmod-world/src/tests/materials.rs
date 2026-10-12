use gmod_bsp::testmap::{TestFace, TestMap, TestOverlay};

use super::*;

/// A game folder with `files`, and its file system.
fn game(name: &str, files: &[(&str, &[u8])]) -> (Game, Vfs) {
    let root = std::env::temp_dir().join(format!("gmod-world-{name}-{}", std::process::id()));
    let game = Game(root.clone());
    for (path, data) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }
    let mut vfs = Vfs::new();
    vfs.push(Mount::new(&["GAME"], Backing::Dir(gmod_fs::DirMount::new(&root))));
    (game, vfs)
}

/// A floor of 128 units from the origin.
fn floor(material: &'static str) -> TestFace {
    TestFace { corners: vec![[0.0, 0.0, 0.0], [0.0, 128.0, 0.0], [128.0, 128.0, 0.0], [128.0, 0.0, 0.0]], material, flags: 0 }
}

#[test]
fn proxies_change_how_surfaces_look() {
    let files: [(&str, &[u8]); 3] = [
        (
            "materials/dev/water.vmt",
            br#"UnlitGeneric
            {
                $basetexture dev/grid
                $basetexturetransform "center .5 .5 scale 2 2 rotate 0 translate 0 0"
                $frame 3
                Proxies
                {
                    AnimatedTexture { animatedtexturevar $basetexture animatedtextureframenumvar $frame animatedtextureframerate 4 }
                    TextureScroll { texturescrollvar $basetexturetransform texturescrollrate 0.5 texturescrollangle 0 }
                    Sine { sinemin 0 sinemax 1 sineperiod 4 resultvar $alpha }
                    PlayerColor { resultvar $color }
                }
            }"#,
        ),
        (
            "materials/dev/still.vmt",
            br#"UnlitGeneric { $basetexture dev/grid $basetexturetransform "scale 2 2" $frame 3 $color "[1 0 0]" }"#,
        ),
        ("materials/dev/grid.vtf", &texture()),
    ];
    let (_game, vfs) = game("proxies", &files);
    let map = TestMap { faces: vec![floor("DEV/WATER"), floor("DEV/STILL")], ..TestMap::default() };
    let bsp = Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    let world = load(&vfs, &bsp);
    let surfaces = &world.pieces[0].surfaces;
    let water = surfaces.iter().find(|s| s.material == "DEV/WATER").unwrap();
    let still = surfaces.iter().find(|s| s.material == "DEV/STILL").unwrap();

    // as the material sets it, and as it stays without proxies
    assert!(still.proxies.is_none() && !still.translucent);
    let look = still.look(10.0);
    assert_eq!((look.frame, look.tint, look.alpha), (3, [1.0, 0.0, 0.0], 1.0));
    assert_eq!(look.transform, [[2.0, 0.0, -0.5], [0.0, 2.0, -0.5]]);

    // the proxies: the frame counts on 4 a second, the texture scrolls along u, the alpha
    // pulses (so it is drawn as translucent), the player's colour is left alone
    assert!(water.translucent, "proxies fade it");
    assert_eq!(water.proxies.as_ref().unwrap().ignored, ["playercolor"]);
    let at = |t: f32| water.look(t);
    assert_eq!((at(0.0).frame, at(0.5).frame, at(1.0).frame), (0, 2, 4));
    assert_eq!(at(0.0).transform, IDENTITY, "the scroll replaces the transform");
    assert_eq!(at(1.0).transform, [[1.0, 0.0, 0.5], [0.0, 1.0, 0.0]]);
    assert!((at(1.0).alpha - 1.0).abs() < 1e-5 && (at(3.0).alpha).abs() < 1e-5);
    assert_eq!(at(1.0).tint, [1.0; 3]);
}

#[test]
fn overlays_are_drawn_with_the_world() {
    let files: [(&str, &[u8]); 4] = [
        ("materials/concrete/floor.vmt", b"LightmappedGeneric { $basetexture dev/grid }"),
        ("materials/decals/sign.vmt", b"LightmappedGeneric { $basetexture decals/sign $translucent 1 $decal 1 }"),
        ("materials/decals/crack.vmt", b"DecalModulate { $basetexture decals/sign }"),
        ("materials/decals/sign.vtf", &texture()),
    ];
    let (_game, vfs) = game("overlays", &files);
    let up = |material, size| TestOverlay::square(material, vec![0], [64.0, 64.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], size);
    let map = TestMap {
        faces: vec![floor("CONCRETE/FLOOR")],
        overlays: vec![up("DECALS/SIGN", 32.0), up("DECALS/CRACK", 16.0)],
        ..TestMap::default()
    };
    let bsp = Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    let world = load(&vfs, &bsp);
    assert_eq!(world.pieces.len(), 1);
    let piece = &world.pieces[0];
    let names: Vec<&str> = piece.surfaces.iter().map(|s| s.material.as_str()).collect();
    assert_eq!(names, ["CONCRETE/FLOOR", "DECALS/SIGN", "DECALS/CRACK"]);
    let overlays: Vec<bool> = piece.surfaces.iter().map(|s| s.overlay).collect();
    assert_eq!(overlays, [false, true, true]);
    let draws: Vec<bool> = crate::gpu::geometry(&world.pieces).draws.iter().map(|d| d.overlay).collect();
    assert_eq!(draws, [false, true, true], "the renderer draws overlays in order");
    assert_eq!(world.triangles(), 2 + 2 + 2);

    // the sign: a square of its own texture, translucent, lifted off the floor and lit by its
    // light map
    let sign = &piece.surfaces[1];
    assert!(sign.translucent && sign.lit && !sign.missing && !sign.modulate);
    assert_eq!(sign.base.map(|t| world.textures[t].name.as_str()), Some("decals/sign"));
    let indices = &piece.indices[sign.first..sign.first + sign.count];
    let corners: Vec<&Vertex> = indices.iter().map(|&i| &piece.vertices[i as usize]).collect();
    assert!(corners.iter().all(|v| v.position[2] == gmod_bsp::overlay::LIFT && v.normal == [0.0, 0.0, 1.0]));
    let xs = corners.iter().map(|v| v.position[0]);
    assert_eq!((xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max)), (48.0, 80.0));
    assert!(corners.iter().any(|v| v.uv == [1.0, 1.0]) && corners.iter().any(|v| v.uv == [0.0, 0.0]));
    // the crack darkens what is under it
    let crack = &piece.surfaces[2];
    assert!(crack.modulate && crack.translucent && !crack.lit);
}
