use std::sync::Arc;

use super::*;
use crate::testmap::{BLUE, GREEN, RED, TestBrush, TestMap};

fn map() -> Bsp {
    Bsp::parse("test.bsp".into(), Arc::from(testmap::bytes())).unwrap()
}

#[test]
fn reads_every_lump() {
    let bsp = map();
    assert_eq!(bsp.version, 20);
    assert_eq!(bsp.entities.len(), 2, "from the LZMA-compressed lump");
    assert_eq!(bsp.worldspawn().and_then(|w| w.get("skyname")), Some("sky_day01_01"));
    assert_eq!(bsp.entities[1].vec3("origin"), Some([64.0, 64.0, 16.0]));
    assert_eq!((bsp.planes.len(), bsp.vertices.len(), bsp.edges.len(), bsp.surfedges.len()), (2, 20, 21, 20));
    assert_eq!(bsp.faces.len(), 5);
    assert_eq!(
        bsp.materials,
        ["DEV/DEV_MEASUREGENERIC01", "BRICK/BRICKWALL001A", "NATURE/BLENDGROUNDTOGRASS", "TOOLS/TOOLSSKYBOX", "TOOLS/TOOLSNODRAW"]
    );
    assert_eq!(bsp.face_material(&bsp.faces[1]), Some("BRICK/BRICKWALL001A"));
    assert_eq!(
        bsp.face_vertices(&bsp.faces[2]).collect::<Vec<_>>(),
        [[256.0, 0.0, 0.0], [256.0, 128.0, 0.0], [384.0, 128.0, 0.0], [384.0, 0.0, 0.0]]
    );
    assert_eq!(bsp.displacements.len(), 1);
    assert_eq!(bsp.displacements[0].side(), 5);
    assert_eq!(bsp.disp_verts.len(), 25);
    assert_eq!(bsp.cubemaps, [Cubemap { origin: [64, 64, 64], size: 0 }]);
    assert_eq!(bsp.static_props.version, 6);
    assert_eq!(bsp.static_props.models, ["models/props_c17/oildrum001.mdl"]);
    assert_eq!(bsp.static_props.props.len(), 1);
    let prop = bsp.static_props.props[0];
    assert_eq!((prop.origin, prop.angles, prop.skin), ([32.0, 64.0, 0.0], [0.0, 90.0, 0.0], 1));
    let pak = bsp.pakfile().unwrap().unwrap();
    assert!(pak.read("materials/maps/test/patched.vmt").unwrap().starts_with(b"\"patch\""));
}

#[test]
fn rejects_what_is_not_a_map() {
    assert!(Bsp::parse("x".into(), Arc::from(b"IBSP".repeat(400))).is_err());
    let mut wrong_version = testmap::bytes();
    wrong_version[4..8].copy_from_slice(&29i32.to_le_bytes());
    assert!(Bsp::parse("x".into(), Arc::from(wrong_version)).is_err());
    // a face pointing at edges that do not exist is caught when reading
    let mut broken = testmap::bytes();
    let faces = u32::from_le_bytes(broken[8 + 7 * 16..8 + 7 * 16 + 4].try_into().unwrap()) as usize;
    broken[faces + 4..faces + 8].copy_from_slice(&1000i32.to_le_bytes());
    let err = Bsp::parse("x".into(), Arc::from(broken)).unwrap_err();
    assert!(err.to_string().contains("edges out of range"), "{err}");
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn lightmap_at(mesh: &mesh::Mesh, uv: [f32; 2]) -> [f32; 3] {
    let x = (uv[0] * mesh.lightmap.width as f32) as usize;
    let y = (uv[1] * mesh.lightmap.height as f32) as usize;
    mesh.lightmap.texels[y * mesh.lightmap.width as usize + x]
}

#[test]
fn world_mesh() {
    let bsp = map();
    let mesh = mesh::build(&bsp, mesh::Options::default());
    assert_eq!(mesh.sky_faces, 1);
    assert_eq!(mesh.broken_faces, 0);
    // floor and wall: 4 vertices and 2 triangles each; the displacement 25 and 32
    assert_eq!(mesh.vertices.len(), 4 + 4 + 25);
    assert_eq!(mesh.indices.len(), (2 + 2 + 32) * 3);
    assert!(mesh.indices.iter().all(|&i| (i as usize) < mesh.vertices.len()));
    let materials: Vec<usize> = mesh.batches.iter().map(|b| b.material).collect();
    assert_eq!(materials, [0, 1, 2]);
    assert_eq!(mesh.batches.iter().map(|b| b.index_count).sum::<u32>() as usize, mesh.indices.len());

    // the lightmaps landed where the vertices look them up
    let to_linear = |c: [u8; 4]| rgbe_to_linear(c);
    for (batch, colour) in mesh.batches.iter().zip([RED, GREEN, BLUE]) {
        for &i in &mesh.indices[batch.first_index as usize..(batch.first_index + batch.index_count) as usize] {
            assert_eq!(lightmap_at(&mesh, mesh.vertices[i as usize].lightmap_uv), to_linear(colour), "material {}", batch.material);
        }
    }

    // floor: texture coordinates over the 512 x 256 texture, normal up
    let floor = &mesh.vertices[0..4];
    assert_eq!(floor[2].uv, [128.0 / 512.0, 128.0 / 256.0]);
    assert!(floor.iter().all(|v| v.normal == [0.0, 0.0, 1.0]));

    // displacement: rows run from the start corner (256, 0, 0) to the next corner (along y),
    // columns across (along x); each column is raised 4 units more, and alpha grows by row
    let disp = &mesh.vertices[8..];
    let at = |row: usize, column: usize| disp[row * 5 + column];
    assert_eq!(at(0, 0).position, [256.0, 0.0, 0.0]);
    assert_eq!(at(4, 0).position, [256.0, 128.0, 0.0]);
    assert_eq!(at(0, 4).position, [384.0, 0.0, 16.0]);
    assert_eq!(at(4, 4).position, [384.0, 128.0, 16.0]);
    assert_eq!(at(2, 1).position, [288.0, 64.0, 4.0]);
    assert!((at(4, 2).alpha - 1.0).abs() < 1e-6 && at(0, 2).alpha == 0.0);
    assert_eq!(at(4, 4).uv, [384.0 / 512.0, 128.0 / 256.0]);
    // a slope rising toward +x: normals lean toward -x and still point up
    assert!(disp.iter().all(|v| v.normal[2] > 0.9 && v.normal[0] < 0.0), "{:?}", at(2, 2).normal);
    // triangles turn clockwise seen from the front, as the game's do
    for tri in mesh.indices.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|k| mesh.vertices[tri[k] as usize]);
        let (u, v) = (sub(b.position, a.position), sub(c.position, a.position));
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        assert!(n[0] * a.normal[0] + n[1] * a.normal[1] + n[2] * a.normal[2] < 0.0, "{tri:?}");
    }
}

#[test]
fn faces_look_along_their_plane() {
    // `side` only says that the plane is the flipped one of a pair; it faces the way the face does
    let mut bytes = testmap::bytes();
    let faces = u32::from_le_bytes(bytes[8 + 7 * 16..8 + 7 * 16 + 4].try_into().unwrap()) as usize;
    bytes[faces + Face::SIZE + 2] = 1; // the wall's side
    let bsp = Bsp::parse("test.bsp".into(), Arc::from(bytes)).unwrap();
    assert_eq!(bsp.faces[1].side, 1);
    let mesh = mesh::build(&bsp, mesh::Options::default());
    assert!(mesh.vertices[4..8].iter().all(|v| v.normal == [0.0, 1.0, 0.0]), "{:?}", mesh.vertices[4].normal);
}

#[test]
fn the_light_for_what_moves() {
    use crate::testmap::{TestBrush, TestLight, TestMap};
    // a cube 1 from +z, a quarter from the rest; a lamp and the sun
    let quarter = [64, 64, 64, (-1i8) as u8];
    let lit = TestMap {
        models: vec![vec![TestBrush::cuboid([-512.0, -512.0, -16.0], [512.0, 512.0, 0.0])]],
        ambient: Some([quarter, quarter, quarter, quarter, [255, 255, 255, 0], quarter]),
        lights: vec![
            TestLight { kind: 1, origin: [0.0, 0.0, 100.0], intensity: [1000.0, 500.0, 0.0], normal: [0.0; 3] },
            TestLight { kind: 3, origin: [0.0; 3], intensity: [2.0; 3], normal: [0.0, 0.0, -1.0] },
        ],
        ..TestMap::default()
    };
    let bsp = Bsp::parse("light.bsp".into(), Arc::from(lit.bytes())).unwrap();
    let lights = bsp.world_lights();
    assert_eq!(lights.len(), 2);
    assert_eq!(
        (lights[0].kind, lights[0].origin, lights[0].intensity, lights[0].quadratic),
        (LightKind::Point, [0.0, 0.0, 100.0], [1000.0, 500.0, 0.0], 1.0)
    );
    assert_eq!((lights[1].kind, lights[1].normal), (LightKind::Sky, [0.0, 0.0, -1.0]));
    let samples = bsp.ambient_samples().unwrap();
    assert_eq!(samples.len(), bsp.leaves.len());
    let sample = samples.iter().flatten().next().expect("a sample");
    assert_eq!(sample.cube[4], [1.0; 3]);
    assert!((sample.cube[0][0] - 64.0 / 255.0 / 2.0).abs() < 1e-6);
    // in the middle of its leaf
    let leaf = bsp.leaves.iter().zip(&samples).find(|(_, s)| !s.is_empty()).unwrap().0;
    assert!((0..3).all(|i| sample.position[i] >= f32::from(leaf.mins[i]) && sample.position[i] <= f32::from(leaf.maxs[i])));
    // a map without them has none
    assert!(map().world_lights().is_empty());
}

#[test]
fn hdr_ambient_light_that_holds_nothing_gives_way_to_ldr() {
    let room = || vec![TestBrush::cuboid([-64.0, -64.0, -16.0], [64.0, 64.0, 0.0])];
    let ldr = [[128, 128, 128, 0]; 6];
    // as gm_construct's: one dark cube everywhere
    let dark = TestMap { models: vec![room()], ambient: Some(ldr), ambient_hdr: Some([[1, 1, 1, 248]; 6]), ..TestMap::default() };
    let bsp = Bsp::parse("dark.bsp".into(), Arc::from(dark.bytes())).unwrap();
    assert_eq!(bsp.ambient_samples().unwrap().iter().flatten().next().unwrap().cube[4], [128.0 / 255.0; 3]);
    // real HDR light is taken first
    let bright = TestMap { models: vec![room()], ambient: Some(ldr), ambient_hdr: Some([[255, 255, 255, 0]; 6]), ..TestMap::default() };
    let bsp = Bsp::parse("bright.bsp".into(), Arc::from(bright.bytes())).unwrap();
    assert_eq!(bsp.ambient_samples().unwrap().iter().flatten().next().unwrap().cube[4], [1.0; 3]);
}
