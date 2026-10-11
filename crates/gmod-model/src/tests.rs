use super::*;

/// Little-endian bytes with fields to fill in later.
#[derive(Default)]
struct Buf(Vec<u8>);

impl Buf {
    fn at(&self) -> usize {
        self.0.len()
    }

    fn i32(&mut self, v: i32) -> &mut Buf {
        self.0.extend(v.to_le_bytes());
        self
    }

    fn f32s(&mut self, v: &[f32]) -> &mut Buf {
        for x in v {
            self.0.extend(x.to_le_bytes());
        }
        self
    }

    fn u16(&mut self, v: u16) -> &mut Buf {
        self.0.extend(v.to_le_bytes());
        self
    }

    fn bytes(&mut self, v: &[u8]) -> &mut Buf {
        self.0.extend_from_slice(v);
        self
    }

    fn zeros(&mut self, n: usize) -> &mut Buf {
        self.0.resize(self.0.len() + n, 0);
        self
    }

    fn set(&mut self, at: usize, v: i32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Appends a NUL-terminated string; returns where it is.
    fn string(&mut self, s: &str) -> usize {
        let at = self.at();
        self.bytes(s.as_bytes()).bytes(&[0]);
        at
    }
}

const CHECKSUM: i32 = 0x1234_5678;

/// A model with one bone, two textures, two skins and one body part of two models: a quad
/// (4 vertices) and a triangle (3).
fn mdl() -> Vec<u8> {
    let mut b = Buf::default();
    b.bytes(b"IDST").i32(48).i32(CHECKSUM);
    b.bytes(&[0; 64]);
    b.0[12..12 + 13].copy_from_slice(b"test/thing.md");
    b.i32(0); // data length
    b.f32s(&[0.0, 0.0, 64.0, 1.0, 2.0, 3.0]); // eye, illumination
    b.f32s(&[-8.0, -8.0, 0.0, 8.0, 8.0, 16.0]); // hull
    b.f32s(&[-9.0, -9.0, 0.0, 9.0, 9.0, 17.0]); // view box
    b.i32(0x11); // flags
    b.zeros(408 - b.at());
    // tables, their offsets filled into the header afterwards
    let bone = b.at();
    // name, parent, controllers; position and rotation; Euler angles and their scales
    b.i32(0).i32(-1).zeros(24).f32s(&[1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]).zeros(36);
    b.f32s(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]).zeros(216 - 144);
    let textures = b.at();
    b.zeros(2 * 64);
    let folders = b.at();
    b.i32(0);
    let skins = b.at();
    b.u16(0).u16(1); // skin 0: slot 0 shows texture 0; skin 1: texture 1
    let part = b.at();
    b.i32(0).i32(2).i32(1).i32(0);
    let models = b.at();
    b.zeros(2 * 148);
    let meshes = b.at();
    b.zeros(2 * 116);
    for (i, (vertices, first_byte)) in [(4, 0), (3, 4 * 48)].into_iter().enumerate() {
        let model = models + i * 148;
        b.0[model..model + 1].copy_from_slice(b"m");
        b.set(model + 72, 1);
        b.set(model + 76, (meshes + i * 116) as i32 - model as i32);
        b.set(model + 80, vertices);
        b.set(model + 84, first_byte);
        let mesh = meshes + i * 116;
        b.set(mesh + 8, vertices);
    }
    // an attachment on the root bone, 4 5 6 along it; a flex
    let attachment = b.at();
    b.i32(0).i32(0).i32(0).f32s(&[1.0, 0.0, 0.0, 4.0, 0.0, 1.0, 0.0, 5.0, 0.0, 0.0, 1.0, 6.0]).zeros(32);
    let flex = b.at();
    b.i32(0);
    let name = b.string("root");
    b.set(bone, (name - bone) as i32);
    let muzzle = b.string("muzzle");
    b.set(attachment, (muzzle - attachment) as i32);
    let smile = b.string("smile");
    b.set(flex, (smile - flex) as i32);
    for (i, texture) in ["metal", "metal_rusty"].into_iter().enumerate() {
        let at = b.string(texture);
        b.set(textures + i * 64, (at - (textures + i * 64)) as i32);
    }
    let folder = b.string("models\\test\\");
    b.set(folders, folder as i32);
    let part_name = b.string("body");
    b.set(part, (part_name - part) as i32);
    b.set(part + 12, (models - part) as i32);
    let surface = b.string("metal");
    let text = "prop_data { \"base\" \"Metal.Small\" }";
    let key_values = b.string(text);
    for (at, v) in [
        (156, 1),
        (160, bone as i32),
        (204, 2),
        (208, textures as i32),
        (212, 1),
        (216, folders as i32),
        (220, 1),
        (224, 2),
        (228, skins as i32),
        (232, 1),
        (236, part as i32),
        (240, 1),
        (244, attachment as i32),
        (260, 1),
        (264, flex as i32),
        (308, surface as i32),
        (312, key_values as i32),
        (316, text.len() as i32 + 1),
    ] {
        b.set(at, v);
    }
    b.0[328..332].copy_from_slice(&25.0f32.to_le_bytes());
    b.0
}

/// 7 vertices (the quad, then the triangle); with `fixups`, the file keeps the triangle's first.
fn vvd(fixups: bool) -> Vec<u8> {
    let vertex = |i: usize| {
        let mut v = Buf::default();
        v.f32s(&[1.0, 0.0, 0.0]).bytes(&[0, 0, 0, 1]);
        v.f32s(&[i as f32, 2.0 * i as f32, 0.0]).f32s(&[0.0, 0.0, 1.0]).f32s(&[i as f32 / 8.0, 0.5]);
        v.0
    };
    let order: Vec<usize> = if fixups { vec![4, 5, 6, 0, 1, 2, 3] } else { (0..7).collect() };
    let mut b = Buf::default();
    b.bytes(b"IDSV").i32(4).i32(CHECKSUM).i32(1).i32(7).zeros(28);
    let fixup_table = 64;
    let fixup_count = if fixups { 2 } else { 0 };
    let vertex_at = fixup_table + fixup_count * 12;
    b.i32(fixup_count).i32(fixup_table).i32(vertex_at).i32(vertex_at + 7 * 48);
    if fixups {
        // level 0 takes the quad (stored last) first, then the triangle
        b.i32(0).i32(3).i32(4).i32(0).i32(0).i32(3);
    }
    for i in order {
        b.bytes(&vertex(i));
    }
    for i in 0..7 {
        b.f32s(&[1.0, 0.0, 0.0, i as f32]);
    }
    b.0
}

/// The triangles: the quad as a triangle list, the triangle as a strip. `newer` writes the
/// larger strip group and strip records of newer compilers.
fn vtx(newer: bool) -> Vec<u8> {
    let (group_size, strip_size) = if newer { (33, 35) } else { (25, 27) };
    let mut b = Buf::default();
    b.i32(7).i32(24).u16(53).u16(9).i32(3).i32(CHECKSUM).i32(1).i32(0).i32(1).i32(36);
    let part = b.at();
    b.i32(2).i32(8);
    let models = b.at();
    b.zeros(16);
    // (group vertices, indices, strip flags) for the two models; the quad's hardware vertices
    // are the mesh's backwards
    let shapes: [(&[u16], &[u16], u8); 2] = [(&[3, 2, 1, 0], &[3, 2, 1, 3, 1, 0], 1), (&[0, 1, 2], &[0, 1, 2], 2)];
    for (m, (vertices, indices, flags)) in shapes.into_iter().enumerate() {
        let level = b.at();
        b.set(models + m * 8, 1);
        b.set(models + m * 8 + 4, (level - (models + m * 8)) as i32);
        b.i32(1).i32(12).f32s(&[0.0]);
        let mesh = b.at();
        b.i32(1).i32(9).bytes(&[0]);
        let group = b.at();
        assert_eq!(group, mesh + 9);
        b.zeros(group_size);
        let strip = b.at();
        b.zeros(strip_size);
        let vertex_at = b.at();
        for &v in vertices {
            b.bytes(&[0, 0, 0, 1]).u16(v).bytes(&[0, 0, 0]);
        }
        let index_at = b.at();
        for &i in indices {
            b.u16(i);
        }
        for (field, v) in
            [(0, vertices.len()), (4, vertex_at - group), (8, indices.len()), (12, index_at - group), (16, 1), (20, strip - group)]
        {
            b.set(group + field, v as i32);
        }
        b.set(strip, indices.len() as i32);
        b.set(strip + 8, vertices.len() as i32);
        b.0[strip + 18] = flags;
    }
    assert_eq!(part, 36);
    b.0
}

#[test]
fn reads_a_model() {
    let model = Model::parse(&mdl(), &vvd(false), &vtx(false)).unwrap();
    let mdl = &model.mdl;
    assert_eq!((mdl.version, mdl.checksum, mdl.flags, mdl.name.as_str()), (48, CHECKSUM, 0x11, "test/thing.md"));
    assert_eq!(mdl.hull, ([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0]));
    assert_eq!(mdl.bones.len(), 1);
    assert_eq!((mdl.bones[0].name.as_str(), mdl.bones[0].parent, mdl.bones[0].position), ("root", -1, [1.0, 2.0, 3.0]));
    assert_eq!(mdl.bones[0].rotation, [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(mdl.textures, ["metal", "metal_rusty"]);
    assert_eq!(mdl.material_candidates(1), ["models/test/metal_rusty"]);
    assert_eq!(mdl.skins, [vec![0], vec![1]]);
    assert_eq!((mdl.surface_prop.as_str(), mdl.mass), ("metal", 25.0));
    assert!(mdl.key_values.starts_with("prop_data"));
    assert_eq!(mdl.attachments.len(), 1);
    assert_eq!((mdl.attachments[0].name.as_str(), mdl.attachments[0].bone, mdl.attachments[0].local[2][3]), ("muzzle", 0, 6.0));
    assert_eq!(mdl.flexes, ["smile"]);
    assert_eq!(mdl.body_parts[0].name, "body");
    assert_eq!(mdl.body_parts[0].models.iter().map(|m| (m.first_vertex, m.vertex_count)).collect::<Vec<_>>(), [(0, 4), (4, 3)]);
    assert_eq!(model.vvd.vertices.len(), 7);
    assert_eq!(model.vvd.tangents[6], [1.0, 0.0, 0.0, 6.0]);

    // body group 0: the quad, skin 0
    let quad = model.mesh(0, 0).unwrap();
    assert_eq!(quad.indices, [0, 1, 2, 0, 2, 3]);
    assert_eq!(quad.batches, [Batch { texture: 0, first_index: 0, index_count: 6 }]);
    assert_eq!(quad.vertices[3].position, [3.0, 6.0, 0.0]);
    assert_eq!(quad.vertices[3].uv, [3.0 / 8.0, 0.5]);
    assert_eq!((quad.vertices[0].bone_count, quad.vertices[0].weights[0]), (1, 1.0));
    // body group 1: the triangle, from a strip; skin 1 shows the other texture
    let triangle = model.mesh(1, 1).unwrap();
    assert_eq!(triangle.indices, [0, 1, 2]);
    assert_eq!(triangle.vertices[0].position, [4.0, 8.0, 0.0]);
    assert_eq!(triangle.batches[0].texture, 1);
    // a skin that is not there falls back to the first
    assert_eq!(model.mesh(0, 9).unwrap().batches[0].texture, 0);
}

/// Baked light for the two models: colour `j` of the 7 is B = 10 j, G = 0, R = 200.
fn vhv(quad_vertices: i32) -> Vec<u8> {
    let mut b = Buf::default();
    b.i32(2).i32(CHECKSUM).i32(4).i32(4).i32(7).i32(2).zeros(16);
    b.i32(0).i32(quad_vertices).i32(96).zeros(16);
    b.i32(0).i32(3).i32(96 + 16).zeros(16);
    b.zeros(96 - b.at());
    for j in 0..7u8 {
        b.bytes(&[10 * j, 0, 200, 255]);
    }
    b.0
}

#[test]
fn baked_light_of_static_props() {
    let model = Model::parse(&mdl(), &vvd(false), &vtx(false)).unwrap();
    assert_eq!(model.vtx.body_parts[0][0][0].meshes[0].vertices, [3, 2, 1, 0]);
    let baked = Vhv::parse(&vhv(4)).unwrap();
    let lit = model.lit_mesh(0, 0, &baked).unwrap();
    let light = lit.light.unwrap();
    let red = 200.0 / 255.0;
    // hardware vertex 0 is mesh vertex 3, hardware vertex 3 mesh vertex 0
    assert_eq!(light[3], [red, 0.0, 0.0]);
    assert_eq!(light[0], [red, 0.0, 30.0 / 255.0]);
    let triangle = model.lit_mesh(1, 0, &baked).unwrap();
    assert_eq!(triangle.light.unwrap()[2], [red, 0.0, 60.0 / 255.0]);
    // light made for another model, or for other vertices, is not used
    assert_eq!(model.lit_mesh(0, 0, &Vhv::parse(&vhv(5)).unwrap()).unwrap().light, None, "5 colours for 4 vertices");
    let mut other = vhv(4);
    other[4] ^= 1;
    assert_eq!(model.lit_mesh(0, 0, &Vhv::parse(&other).unwrap()).unwrap().light, None);
    let mut miscounted = Vhv::parse(&vhv(4)).unwrap();
    miscounted.meshes[0].colors.pop();
    assert_eq!(model.lit_mesh(0, 0, &miscounted).unwrap().light, None);
    assert!(Vhv::parse(b"short").is_err());
}

#[test]
fn newer_layouts_and_fixups() {
    let older = Model::parse(&mdl(), &vvd(false), &vtx(false)).unwrap();
    let newer = Model::parse(&mdl(), &vvd(true), &vtx(true)).unwrap();
    assert_eq!(older.vtx, newer.vtx, "the 33- and 35-byte records read the same");
    assert_eq!(older.vvd.vertices, newer.vvd.vertices, "fixups restore the order");
    assert_eq!(newer.vvd.tangents[0], [1.0, 0.0, 0.0, 3.0], "tangents follow their vertices");
}

#[test]
fn rejects_what_does_not_fit() {
    assert!(Mdl::parse(b"IDSV and more").is_err());
    let mut version = mdl();
    version[4] = 30;
    assert!(Mdl::parse(&version).is_err());
    // a table that runs past the end of the file
    let mut bones = mdl();
    bones[156..160].copy_from_slice(&100_000i32.to_le_bytes());
    let err = Mdl::parse(&bones).unwrap_err().to_string();
    assert!(err.contains("bones"), "{err}");
    // files of different models
    let mut other = vvd(false);
    other[8] ^= 1;
    let model = Model::parse(&mdl(), &other, &vtx(false)).unwrap();
    assert!(model.mesh(0, 0).unwrap_err().to_string().contains("do not belong together"));
    // a triangle index past the group's vertices
    let mut broken = vtx(false);
    let last = broken.len() - 2;
    broken[last] = 9;
    assert!(Vtx::parse(&broken).is_err());
    assert!(Vtx::parse(&broken[..40]).is_err());
}

const PHY_TEXT: &str = "solid {\n\"index\" \"0\"\n\"name\" \"static_prop\"\n\"mass\" \"25.000000\"\n\"surfaceprop\" \"metal\"\n\"damping\" \"0.000000\"\n\"volume\" \"4096.0\"\n}\nsolid {\n\"index\" \"1\"\n\"name\" \"arm\"\n\"mass\" \"5\"\n}\nragdollconstraint {\n\"parent\" \"0\"\n\"child\" \"1\"\n\"xmin\" \"-30\"\n\"xmax\" \"45\"\n\"xfriction\" \"2\"\n\"ymin\" \"-10\"\n\"ymax\" \"10\"\n\"zmin\" \"0\"\n\"zmax\" \"90\"\n}\neditparams {\n\"rootname\" \"\"\n\"totalmass\" \"30\"\n\"concave\" \"1\"\n}\n";

fn tetrahedron(at: [f32; 3]) -> testmodel::TestConvex {
    let points = [[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 16.0, 0.0], [0.0, 0.0, 16.0]];
    testmodel::TestConvex {
        points: points.map(|p| [p[0] + at[0], p[1] + at[1], p[2] + at[2]]).to_vec(),
        triangles: vec![[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]],
    }
}

/// A box of 16 × 16 × 16 on the floor, and an arm of three pieces.
fn phy_bytes(legacy: bool) -> Vec<u8> {
    let solids = vec![
        vec![testmodel::TestConvex::cuboid([-8.0, -8.0, 0.0], [8.0, 8.0, 16.0])],
        vec![tetrahedron([0.0; 3]), testmodel::TestConvex::cuboid([20.0, 0.0, 0.0], [30.0, 4.0, 4.0]), tetrahedron([0.0, 0.0, 40.0])],
    ];
    testmodel::phy(CHECKSUM, &solids, legacy, PHY_TEXT)
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
}

#[test]
fn reads_physics_models() {
    for legacy in [false, true] {
        let phy = Phy::parse(&phy_bytes(legacy)).unwrap();
        assert_eq!((phy.checksum, phy.solids.len()), (CHECKSUM, 2));
        // the box, back from IVP's metres and axes
        let cube = &phy.solids[0].convexes;
        assert_eq!(cube.len(), 1);
        assert_eq!((cube[0].points.len(), cube[0].triangles.len()), (8, 12));
        for corner in [[-8.0, -8.0, 0.0], [8.0, 8.0, 16.0], [8.0, -8.0, 16.0]] {
            assert!(cube[0].points.iter().any(|&p| near(p, corner)), "{corner:?} in {:?}", cube[0].points);
        }
        // every triangle faces out
        for convex in phy.solids.iter().flat_map(|s| &s.convexes) {
            let n = convex.points.len() as f32;
            let middle = convex.points.iter().fold([0.0; 3], |m, p| [m[0] + p[0] / n, m[1] + p[1] / n, m[2] + p[2] / n]);
            for tri in &convex.triangles {
                let [a, b, c] = tri.map(|i| convex.points[i as usize]);
                let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
                let normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let out = [a[0] - middle[0], a[1] - middle[1], a[2] - middle[2]];
                assert!(normal[0] * out[0] + normal[1] * out[1] + normal[2] * out[2] > 0.0, "{tri:?} of {convex:?}");
            }
        }
        // the arm's three pieces, in order, without the hull its tree's inner nodes point to
        let arm = &phy.solids[1].convexes;
        assert_eq!(arm.iter().map(|c| c.points.len()).collect::<Vec<_>>(), [4, 8, 4]);
        assert!(arm[2].points.iter().any(|&p| near(p, [0.0, 0.0, 56.0])));
        // the text
        let solid = &phy.solids[0];
        assert_eq!((solid.name(), solid.mass(), solid.surface_prop()), ("static_prop", Some(25.0), Some("metal")));
        assert_eq!(solid.keys.number("volume"), Some(4096.0));
        assert_eq!((phy.solids[1].name(), phy.solids[1].surface_prop()), ("arm", None));
        assert_eq!(phy.block("editparams").and_then(|b| b.number("totalmass")), Some(30.0));
        assert_eq!(
            phy.ragdoll_constraints(),
            [RagdollConstraint { parent: 0, child: 1, limits: [[-30.0, 45.0], [-10.0, 10.0], [0.0, 90.0]], friction: [2.0, 0.0, 0.0] }]
        );
    }
}

#[test]
fn the_text_of_physics_models_is_read_leniently() {
    let text = "garbage solid { \"index\" \"0\" \"name\" \"a\" nested { \"x\" \"1\" } \"mass\" \"3\" } { stray } break { \"model\" \"gibs/a.mdl\" }";
    let solids = vec![vec![testmodel::TestConvex::cuboid([0.0; 3], [1.0; 3])]];
    let phy = Phy::parse(&testmodel::phy(CHECKSUM, &solids, false, text)).unwrap();
    assert_eq!((phy.solids[0].name(), phy.solids[0].mass()), ("a", Some(3.0)));
    assert_eq!(phy.block("break").and_then(|b| b.get("model")), Some("gibs/a.mdl"));
    assert!(phy.ragdoll_constraints().is_empty());
    assert!(phy.collision_pairs().is_none());
}

#[test]
fn the_parts_of_a_ragdoll_that_hit_each_other() {
    let solids = vec![vec![testmodel::TestConvex::cuboid([0.0; 3], [1.0; 3])]; 3];
    let rules = |text: &str| Phy::parse(&testmodel::phy(CHECKSUM, &solids, false, text)).unwrap().collision_pairs();
    // the pairs named, but not those of solids it does not have
    assert_eq!(
        rules("collisionrules { \"collisionpair\" \"2,0\" \"collisionpair\" \" 1 , 2\" \"collisionpair\" \"1,9\" }"),
        Some(vec![[2, 0], [1, 2]])
    );
    assert_eq!(rules("collisionrules { \"selfcollisions\" \"0\" \"collisionpair\" \"0,1\" }"), Some(vec![]));
    // no rules: all of them hit each other
    assert_eq!(rules(""), None);
}

#[test]
fn rejects_physics_that_does_not_fit() {
    let good = phy_bytes(false);
    assert!(Phy::parse(&good[..good.len() / 3]).is_err());
    assert!(Phy::parse(&[0; 8]).is_err());
    // the first solid: its data at 20, the surface after the 28-byte header
    let surface = 20 + 28;
    let error = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = good.clone();
        change(&mut bytes);
        Phy::parse(&bytes).unwrap_err().to_string()
    };
    // without "VPHY" the header is read as an old model's surface, which does not fit
    assert!(!error(&|b| b[20] = b'X').is_empty());
    assert!(error(&|b| b[26] = 1).contains("type 1"));
    // the root of the tree pointing back
    let root = surface + i32::from_le_bytes(good[surface + 32..surface + 36].try_into().unwrap()) as usize;
    assert!(error(&|b| b[root..root + 4].copy_from_slice(&(-28i32).to_le_bytes())).contains("points back"));
    // a triangle using a point past the end
    let first_edge = surface + 48 + 16 + 4;
    assert!(error(&|b| b[first_edge..first_edge + 2].copy_from_slice(&60000u16.to_le_bytes())).contains("past the end"));
}

/// A game folder in the temporary directory, removed when dropped.
struct Game(std::path::PathBuf);

impl Drop for Game {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn physics_belongs_to_its_model() {
    let game = Game(std::env::temp_dir().join(format!("gmod-model-{}", std::process::id())));
    let file = game.0.join("models/test/box.phy");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, phy_bytes(false)).unwrap();
    let mut vfs = Vfs::new();
    vfs.push(gmod_fs::Mount::new(&["GAME"], gmod_fs::Backing::Dir(gmod_fs::DirMount::new(&game.0))));
    let phy = load_physics(&vfs, "models/test/box.mdl", CHECKSUM).unwrap().unwrap();
    assert_eq!(phy.solids.len(), 2);
    let other = load_physics(&vfs, "models/test/box.mdl", CHECKSUM + 1).unwrap_err().to_string();
    assert!(other.contains("models/test/box.phy") && other.contains("another build"), "{other}");
    assert!(load_physics(&vfs, "models/test/none.mdl", CHECKSUM).unwrap().is_none());
}

use crate::anim::{AutoLayer, PoseParameter, euler_quaternion, flags};
use crate::pose::{IDENTITY, transform};
use crate::testmodel::{Keep, TestAnimation, TestBone, TestRig, TestSequence};

fn bone(name: &str, parent: i32, position: [f32; 3]) -> TestBone {
    TestBone { name: name.to_string(), parent, position, euler: [0.0; 3] }
}

/// One frame of one bone, the same for `frames` frames, kept as `keep` says.
fn still(name: &str, bone: usize, position: [f32; 3], euler: [f32; 3], frames: usize, keep: Keep) -> TestAnimation {
    TestAnimation { frames: vec![vec![(bone, Some(position), euler)]; frames], keep, ..TestAnimation::turning(name, bone, 1, 0.0, 0.0) }
}

/// An arm of three bones - root, upper (10 up), lower (10 along the upper's x) - with
/// animations kept every way a model keeps them, and sequences that play them.
fn arm() -> TestRig {
    let mut long = TestAnimation::turning("@long", 1, 71, 0.0, 2.0);
    long.section_frames = 30;
    let mut blocked = TestAnimation::turning("@blocked", 1, 11, 0.0, -1.0);
    blocked.block = 1;
    let mut delta = still("@delta", 2, [0.0; 3], [0.0, 0.0, 0.25], 2, Keep::Streams);
    delta.flags = flags::DELTA;
    let mut walk = TestAnimation::turning("@walk", 1, 31, 0.0, 0.0);
    walk.moves = Some([60.0, 0.0, 0.0]);
    let mut aim = TestSequence::new("aim", "", 5);
    aim.animations = vec![5, 6, 7];
    aim.blend = [3, 1];
    aim.parameters = [0, -1];
    aim.parameter_start = [-45.0, 0.0];
    aim.parameter_end = [45.0, 0.0];
    let mut bend = TestSequence::new("bend", "", 8);
    bend.flags = flags::DELTA;
    let mut bent = TestSequence::new("idle_bent", "", 0);
    bent.layers = vec![AutoLayer { sequence: 6, pose: None, flags: 0, start: 0.0, peak: 0.0, tail: 0.0, end: 0.0 }];
    let mut idle = TestSequence::new("idle", "ACT_VM_IDLE", 0);
    idle.flags = flags::LOOPING;
    idle.weights = vec![1.0; 3];
    TestRig {
        name: "test/arm.mdl".into(),
        checksum: 99,
        bones: vec![bone("root", -1, [0.0; 3]), bone("upper", 0, [0.0, 0.0, 10.0]), bone("lower", 1, [10.0, 0.0, 0.0])],
        animations: vec![
            TestAnimation::turning("@idle", 1, 11, 0.0, 1.0),
            still("@raw64", 2, [10.0, 1.0, 2.0], [0.3, -0.2, 0.5], 2, Keep::Raw64),
            still("@raw48", 2, [10.0, 1.0, 2.0], [0.3, -0.2, 0.5], 2, Keep::Raw48),
            long,
            blocked,
            still("@aim_left", 0, [0.0; 3], [0.0, 0.0, -0.5], 2, Keep::Streams),
            still("@aim_middle", 0, [0.0; 3], [0.0; 3], 2, Keep::Streams),
            still("@aim_right", 0, [0.0; 3], [0.0, 0.0, 0.5], 2, Keep::Streams),
            delta,
            walk,
        ],
        sequences: vec![
            idle,
            TestSequence::new("raw64", "ACT_VM_PRIMARYATTACK", 1),
            TestSequence::new("raw48", "ACT_VM_PRIMARYATTACK", 2),
            TestSequence::new("long", "", 3),
            TestSequence::new("blocked", "", 4),
            aim,
            bend,
            bent,
            TestSequence::new("walk", "ACT_WALK", 9),
        ],
        pose_parameters: vec![PoseParameter { name: "aim_yaw".into(), flags: 0, start: -45.0, end: 45.0, wrap: 0.0 }],
        attachments: vec![("tip".into(), 2, [5.0, 0.0, 0.0])],
        animation_file: "models/test/arm.ani".into(),
        ..TestRig::default()
    }
}

fn arm_rig() -> Rig {
    let (mdl, ani) = arm().files();
    Rig::new(Mdl::parse(&mdl).unwrap(), ani)
}

/// The angle a rotation turns about z, for rotations about z only.
fn yaw(q: [f32; 4]) -> f32 {
    2.0 * q[2].atan2(q[3])
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn reads_sequences_and_animations() {
    let (bytes, ani) = arm().files();
    assert!(ani.as_ref().is_some_and(|a| a.starts_with(b"IDAG")));
    let mdl = Mdl::parse(&bytes).unwrap();
    assert_eq!(mdl.bones[1].euler, [0.0; 3]);
    assert_eq!(mdl.bones[1].rotation_scale, [testmodel::ROTATION_SCALE; 3]);
    assert_eq!(mdl.sequences.len(), 9);
    let idle = &mdl.sequences[0];
    assert_eq!((idle.label.as_str(), idle.activity.as_str(), idle.activity_weight), ("idle", "ACT_VM_IDLE", 1));
    assert!(idle.looping() && !idle.delta());
    assert_eq!(idle.weights, [1.0; 3]);
    assert_eq!(
        (mdl.sequences[5].animations.clone(), mdl.sequences[5].blend, mdl.sequences[5].parameters),
        (vec![5, 6, 7], [3, 1], [Some(0), None])
    );
    assert!(mdl.sequences[6].delta());
    assert_eq!(mdl.sequences[7].layers[0].sequence, 6);
    let anim = &mdl.animations[0];
    assert_eq!((anim.name.as_str(), anim.fps, anim.frames), ("@idle", 30.0, 11));
    assert_eq!((mdl.animations[3].section_frames, mdl.animations[3].sections.len()), (30, 71 / 30 + 2));
    assert_eq!(mdl.animations[9].movements.last().map(|m| m.position), Some([60.0, 0.0, 0.0]));
    assert_eq!(mdl.pose_parameters[0].name, "aim_yaw");
    assert_eq!(mdl.animation_file.as_deref(), Some("models/test/arm.ani"));
    assert_eq!(mdl.animation_blocks.len(), 2);
}

#[test]
fn poses_follow_the_frames() {
    let rig = arm_rig();
    let params = rig.default_pose_parameters();
    // the rest pose undoes what the model was made in: nothing moves
    let rest = rig.skinning(&rig.bone_to_model(&rig.rest()));
    assert!(rest.iter().flatten().flatten().zip(IDENTITY.iter().flatten().cycle()).all(|(a, b)| close(*a, *b, 1e-5)));

    // idle turns the upper bone from 0 to 1 radian over 10 frames; between frames, between them
    let step = testmodel::ROTATION_SCALE;
    for (cycle, angle) in [(0.0, 0.0), (0.5, 0.5), (0.55, 0.55), (1.0, 1.0)] {
        let pose = rig.pose(0, cycle, &params, &[]);
        assert!(close(yaw(pose[1].rotation), angle, step), "at {cycle}: {}", yaw(pose[1].rotation));
        assert_eq!(pose[1].position, [0.0, 0.0, 10.0], "a bone not moved stays where it is");
    }
    // the lower bone goes with it: 10 along the turned x, 10 up
    let bones = rig.bone_to_model(&rig.pose(0, 0.5, &params, &[]));
    let tip = transform(&bones[2], [0.0; 3]);
    assert!((0..3).all(|i| close(tip[i], [10.0 * 0.5f32.cos(), 10.0 * 0.5f32.sin(), 10.0][i], 1e-2)), "{tip:?}");
    let attachment = rig.attachment(0, &bones).map(|m| transform(&m, [0.0; 3])).unwrap();
    assert!(close(attachment[0], 15.0 * 0.5f32.cos(), 1e-2), "{attachment:?}");

    // a rotation and a position for every frame, in 64 and in 48 bits
    let expected = euler_quaternion([0.3, -0.2, 0.5]);
    for (sequence, tolerance) in [(1, 1e-5), (2, 1e-4)] {
        let pose = rig.pose(sequence, 0.5, &params, &[]);
        let q = pose[2].rotation;
        let dot = (0..4).map(|i| q[i] * expected[i]).sum::<f32>().abs();
        assert!(1.0 - dot < tolerance, "{sequence}: {q:?} for {expected:?}");
        assert!((0..3).all(|i| close(pose[2].position[i], [10.0, 1.0, 2.0][i], 1e-2)), "{:?}", pose[2].position);
    }

    // a long animation in sections of 30 frames, the last one on its own; and one in a block
    for frame in [0, 29, 30, 31, 45, 60, 69, 70] {
        let pose = rig.pose(3, frame as f32 / 70.0, &params, &[]);
        assert!(close(yaw(pose[1].rotation), 2.0 * frame as f32 / 70.0, step * 2.0), "frame {frame}: {}", yaw(pose[1].rotation));
    }
    assert!(close(yaw(rig.pose(4, 1.0, &params, &[])[1].rotation), -1.0, step));
    assert_eq!(rig.read_all().unwrap(), 11 + 2 + 2 + 71 + 11 + 2 * 3 + 2 + 31);
}

#[test]
fn sequences_blend_and_layer() {
    let rig = arm_rig();
    // the aim grid along aim_yaw from -45 to 45: -0.5, 0 and 0.5 radians about z
    let aim = |value: f32| yaw(rig.pose(5, 0.0, &[rig.pose_parameters()[0].normalize(value)], &[])[0].rotation);
    let step = testmodel::ROTATION_SCALE;
    assert!(close(aim(-45.0), -0.5, step) && close(aim(0.0), 0.0, step) && close(aim(45.0), 0.5, step));
    assert!(close(aim(22.5), 0.25, step), "half way between the middle and the right: {}", aim(22.5));
    // without a value given, the parameter is at 0: the middle
    assert!(close(yaw(rig.pose(5, 0.0, &[], &[])[0].rotation), 0.0, step));

    // the bend is a delta: played on idle by its layer it turns the lower bone a quarter
    // radian more, and the upper bone is idle's
    let params = rig.default_pose_parameters();
    let pose = rig.pose(7, 0.5, &params, &[]);
    assert!(close(yaw(pose[2].rotation), 0.25, step) && close(yaw(pose[1].rotation), 0.5, step), "{pose:?}");
    // a layer given (a gesture) weighs in: half the bend
    let pose = rig.pose(0, 0.0, &params, &[Layer { sequence: 6, cycle: 0.0, weight: 0.5 }]);
    assert!(close(yaw(pose[2].rotation), 0.125, step), "{}", yaw(pose[2].rotation));

    // how long sequences take and how fast they move along the ground
    assert!(close(rig.duration(0, &params), 10.0 / 30.0, 1e-6));
    assert!(close(rig.duration(8, &params), 1.0, 1e-6) && close(rig.ground_speed(8, &params), 60.0, 1e-3));
    assert_eq!(rig.ground_speed(0, &params), 0.0);
    assert_eq!(rig.duration(1, &params), 1.0 / 30.0, "two frames: one step");
    assert_eq!(rig.find_sequence("WALK"), Some(8));
}

#[test]
fn included_models_play_on_the_bones_of_the_same_names() {
    // the included model has the bones in another order, one the rig does not have, and
    // a sequence of the same name as the rig's (left out) and one that overrides one
    let mut wave = TestAnimation::turning("@wave", 2, 3, 0.0, 0.6);
    wave.frames.iter_mut().flatten().for_each(|(bone, ..)| *bone = 2);
    let mut raw = TestSequence::new("raw64", "", 0);
    raw.flags = flags::OVERRIDE;
    let anims = TestRig {
        name: "test/anims.mdl".into(),
        bones: vec![
            bone("lower", 2, [10.0, 0.0, 0.0]),
            bone("root", -1, [0.0; 3]),
            bone("upper", 1, [0.0, 0.0, 10.0]),
            bone("tail", 1, [0.0; 3]),
        ],
        animations: vec![wave],
        sequences: vec![TestSequence::new("wave", "ACT_WAVE", 0), TestSequence::new("idle", "", 0), raw],
        pose_parameters: vec![
            PoseParameter { name: "AIM_YAW".into(), flags: 0, start: -45.0, end: 45.0, wrap: 0.0 },
            PoseParameter { name: "lean".into(), flags: 0, start: -1.0, end: 1.0, wrap: 0.0 },
        ],
        ..TestRig::default()
    };
    let mut main = arm();
    main.include_models = vec!["models/test/anims.mdl".into(), "models/test/gone.mdl".into(), "models/test/arm.mdl".into()];

    let game = Game(std::env::temp_dir().join(format!("gmod-model-rig-{}", std::process::id())));
    std::fs::create_dir_all(game.0.join("models/test")).unwrap();
    let (mdl, ani) = main.files();
    std::fs::write(game.0.join("models/test/arm.mdl"), mdl).unwrap();
    std::fs::write(game.0.join("models/test/arm.ani"), ani.unwrap()).unwrap();
    std::fs::write(game.0.join("models/test/anims.mdl"), anims.files().0).unwrap();
    let mut vfs = Vfs::new();
    vfs.push(gmod_fs::Mount::new(&["GAME"], gmod_fs::Backing::Dir(gmod_fs::DirMount::new(&game.0))));
    let rig = Rig::load(&vfs, "models/test/arm.mdl").unwrap();

    // the rig's nine, and wave: idle stays the rig's own, raw64 is the included model's
    assert_eq!(rig.sequence_count(), 10);
    assert_eq!(rig.sequence(0).map(|s| s.animations[0]), Some(0));
    assert_eq!(rig.sequence(1).map(|s| s.flags), Some(flags::OVERRIDE));
    assert_eq!(rig.pose_parameters().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["aim_yaw", "lean"]);
    let wave = rig.find_sequence("wave").unwrap();
    let pose = rig.pose(wave, 1.0, &rig.default_pose_parameters(), &[]);
    assert!(close(yaw(pose[1].rotation), 0.6, testmodel::ROTATION_SCALE), "the included model's upper is the rig's: {pose:?}");
    // the rig's own animations from its .ani still read
    assert!(close(yaw(rig.pose(4, 1.0, &[], &[])[1].rotation), -1.0, testmodel::ROTATION_SCALE));
}

#[test]
fn animations_that_do_not_fit_are_errors_not_poses() {
    let (mdl, ani) = arm().files();
    // the .ani cut short: the blocked animation cannot be read, the others can; one that is
    // not an .ani is left out when the rig is loaded
    let game = Game(std::env::temp_dir().join(format!("gmod-model-ani-{}", std::process::id())));
    std::fs::create_dir_all(game.0.join("models/test")).unwrap();
    std::fs::write(game.0.join("models/test/arm.mdl"), &mdl).unwrap();
    std::fs::write(game.0.join("models/test/arm.ani"), b"not an .ani").unwrap();
    let mut vfs = Vfs::new();
    vfs.push(gmod_fs::Mount::new(&["GAME"], gmod_fs::Backing::Dir(gmod_fs::DirMount::new(&game.0))));
    let loaded = Rig::load(&vfs, "models/test/arm.mdl").unwrap();
    assert!(loaded.read_all().is_err() && close(yaw(loaded.pose(0, 1.0, &[], &[])[1].rotation), 1.0, testmodel::ROTATION_SCALE));
    let ani = ani.unwrap();
    let rig = Rig::new(Mdl::parse(&mdl).unwrap(), Some(ani[..90].to_vec()));
    assert!(rig.read_all().is_err());
    assert_eq!(rig.pose(4, 1.0, &[], &[])[1], rig.rest()[1], "what cannot be read does not move");
    assert!(close(yaw(rig.pose(0, 1.0, &[], &[])[1].rotation), 1.0, testmodel::ROTATION_SCALE));
    // without the .ani at all
    assert!(Rig::new(Mdl::parse(&mdl).unwrap(), None).read_all().is_err());

    // a record for a bone the model does not have moves nothing, as the engine takes it (an
    // animation that moves no bone has one for bone 255); one past the file is an error
    let mut other = mdl.clone();
    let parsed = Mdl::parse(&mdl).unwrap();
    let first = parsed.animations[0].sections[0].at;
    other[first] = 255;
    let rig = Rig::new(Mdl::parse(&other).unwrap(), None);
    assert_eq!(rig.pose(0, 1.0, &[], &[])[1], rig.rest()[1]);
    other[first + 2..first + 4].copy_from_slice(&i16::MAX.to_le_bytes());
    let error = Rig::new(Mdl::parse(&other).unwrap(), None).read_all().unwrap_err().to_string();
    assert!(error.contains("past the end"), "{error}");
    // a sequence playing an animation that is not there
    let mut model = arm();
    model.sequences[0].animations = vec![40];
    assert!(Mdl::parse(&model.files().0).unwrap_err().to_string().contains("animation 40"));
}
