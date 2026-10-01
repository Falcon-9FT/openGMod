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
    let name = b.string("root");
    b.set(bone, (name - bone) as i32);
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
