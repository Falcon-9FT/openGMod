use super::*;
use crate::testmap::{TestBrush, TestFace, TestOverlay};

/// A floor of 128 units from the origin, and a wall along x at y = 128.
fn faces() -> Vec<TestFace> {
    let floor = vec![[0.0, 0.0, 0.0], [0.0, 128.0, 0.0], [128.0, 128.0, 0.0], [128.0, 0.0, 0.0]];
    let wall = vec![[0.0, 128.0, 0.0], [0.0, 128.0, 128.0], [128.0, 128.0, 128.0], [128.0, 128.0, 0.0]];
    vec![TestFace { corners: floor, material: "CONCRETE/FLOOR", flags: 0 }, TestFace { corners: wall, material: "BRICK/WALL", flags: 0 }]
}

fn with(overlays: Vec<TestOverlay>) -> Bsp {
    Bsp::parse("t.bsp".into(), Arc::from(TestMap { faces: faces(), overlays, ..TestMap::default() }.bytes())).unwrap()
}

fn up(origin: [f32; 3], size: f32) -> TestOverlay {
    TestOverlay::square("DECALS/SIGN", vec![0], origin, [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], size)
}

#[test]
fn overlay_records_read_as_written() {
    let mut overlay = up([1.0, 2.0, 3.0], 10.0).overlay;
    overlay.faces = vec![4, 7, 9];
    overlay.render_order = 2;
    overlay.id = 12;
    let bytes = overlay.bytes();
    assert_eq!(bytes.len(), Overlay::SIZE);
    assert_eq!(Overlay::parse(&mut lumps::Fields::new(&bytes)), overlay);

    let bsp = with(vec![up([64.0, 64.0, 0.0], 32.0)]);
    assert_eq!(bsp.overlays.len(), 1);
    let read = &bsp.overlays[0];
    assert_eq!((read.faces.as_slice(), read.origin, read.normal), ([0].as_slice(), [64.0, 64.0, 0.0], [0.0, 0.0, 1.0]));
    assert_eq!(bsp.materials[bsp.texinfo[read.texinfo as usize].texdata as usize], "DECALS/SIGN");
    // U along x, V the normal across it: along y
    assert_eq!(read.axes(), ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]));
    let corners = read.corners();
    assert_eq!(corners[0], ([48.0, 48.0, 0.0], [0.0, 0.0]));
    assert_eq!(corners[1], ([48.0, 80.0, 0.0], [0.0, 1.0]));
    assert_eq!(corners[2], ([80.0, 80.0, 0.0], [1.0, 1.0]));
}

#[test]
fn overlays_lie_on_their_faces() {
    let bsp = with(vec![up([64.0, 64.0, 0.0], 32.0), up([64.0, 64.0, 0.0], 256.0)]);
    let mesh = mesh::build(&bsp, mesh::Options::default());
    let overlays: Vec<&mesh::Batch> = mesh.batches.iter().filter(|b| b.overlay).collect();
    assert_eq!(overlays.len(), 2);
    assert_eq!(mesh.batches.iter().filter(|b| !b.overlay).count(), 2, "the floor and the wall");
    assert_eq!(bsp.materials[overlays[0].material], "DECALS/SIGN");

    let vertices = |batch: &mesh::Batch| -> Vec<mesh::Vertex> {
        let indices = &mesh.indices[batch.first_index as usize..(batch.first_index + batch.index_count) as usize];
        indices.iter().map(|&i| mesh.vertices[i as usize]).collect()
    };
    // the small one whole, lifted off the floor, facing up and wound as faces are
    let small = vertices(overlays[0]);
    assert_eq!(small.len(), 6);
    for v in &small {
        assert_eq!(v.position[2], crate::overlay::LIFT);
        assert_eq!(v.normal, [0.0, 0.0, 1.0]);
        assert!((48.0..=80.0).contains(&v.position[0]) && (48.0..=80.0).contains(&v.position[1]));
    }
    for t in small.chunks(3) {
        let (ab, ac) = (sub(t[1].position, t[0].position), sub(t[2].position, t[0].position));
        assert!(math::cross(ac, ab)[2] > 0.0, "turned away: {t:?}");
    }
    let corner = small.iter().find(|v| v.position[0] == 48.0 && v.position[1] == 80.0).unwrap();
    assert_eq!(corner.uv, [0.0, 1.0]);

    // the big one cut to the floor: its texture from a quarter to three quarters
    let big = vertices(overlays[1]);
    assert!(big.iter().all(|v| (-0.02..=128.02).contains(&v.position[0]) && (-0.02..=128.02).contains(&v.position[1])));
    let corner = big.iter().find(|v| v.position[0] < 1.0 && v.position[1] < 1.0).unwrap();
    assert!((corner.uv[0] - 0.25).abs() < 1e-3 && (corner.uv[1] - 0.25).abs() < 1e-3, "{corner:?}");
    let corner = big.iter().find(|v| v.position[0] > 127.0 && v.position[1] > 127.0).unwrap();
    assert!((corner.uv[0] - 0.75).abs() < 1e-3 && (corner.uv[1] - 0.75).abs() < 1e-3, "{corner:?}");
}

#[test]
fn overlays_on_walls_and_nowhere() {
    // on the wall, its U along x: its V runs up
    let sign = TestOverlay::square("DECALS/SIGN", vec![1], [64.0, 128.0, 64.0], [0.0, -1.0, 0.0], [1.0, 0.0, 0.0], 32.0);
    // on faces that are not there, or the wrong way
    let lost = TestOverlay::square("DECALS/SIGN", vec![-1, 99], [64.0, 64.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 32.0);
    let edge_on = TestOverlay::square("DECALS/SIGN", vec![0], [64.0, 64.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 32.0);
    let bsp = with(vec![sign, lost, edge_on]);
    let mesh = mesh::build(&bsp, mesh::Options::default());
    let overlays: Vec<&mesh::Batch> = mesh.batches.iter().filter(|b| b.overlay).collect();
    assert_eq!(overlays.len(), 1);
    let first = mesh.indices[overlays[0].first_index as usize] as usize;
    let v = mesh.vertices[first];
    assert!((v.position[1] - (128.0 - crate::overlay::LIFT)).abs() < 1e-4, "{v:?}");
    assert_eq!(v.normal, [0.0, -1.0, 0.0]);

    // a lump that does not add up costs the overlays, not the map
    let mut bytes = TestMap { faces: faces(), overlays: vec![up([64.0, 64.0, 0.0], 32.0)], ..TestMap::default() }.bytes();
    let at = 8 + lumps::index::OVERLAYS * 16 + 4;
    let length = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    bytes[at..at + 4].copy_from_slice(&(length - 1).to_le_bytes());
    let bsp = Bsp::parse("t.bsp".into(), Arc::from(bytes)).unwrap();
    assert!(bsp.overlays.is_empty());
}

#[test]
fn overlays_skip_edges_of_no_length() {
    // a wall facing -y whose left edge has a vertex at half height written twice
    let wall =
        vec![[128.0, 128.0, 128.0], [128.0, 128.0, 0.0], [0.0, 128.0, 0.0], [0.0, 128.0, 64.0], [0.0, 128.0, 64.0], [0.0, 128.0, 128.0]];
    let sign = TestOverlay::square("DECALS/SIGN", vec![0], [64.0, 128.0, 64.0], [0.0, -1.0, 0.0], [1.0, 0.0, 0.0], 32.0);
    let map =
        TestMap { faces: vec![TestFace { corners: wall, material: "BRICK/WALL", flags: 0 }], overlays: vec![sign], ..TestMap::default() };
    let bsp = Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    let polygon = crate::overlay::on_face(&bsp, &bsp.overlays[0], &bsp.faces[0]);
    let (low, high) = polygon.iter().fold((f32::MAX, f32::MIN), |(low, high), (p, _)| (low.min(p[2]), high.max(p[2])));
    assert!((low - 48.0).abs() < 1e-3 && (high - 80.0).abs() < 1e-3, "{polygon:?}");
}

#[test]
fn brush_entities_carry_no_overlays() {
    // a door's face in its own space; an overlay names it and the floor
    let door = TestFace {
        corners: vec![[100.0, -32.0, 0.0], [100.0, 32.0, 0.0], [100.0, 32.0, 64.0], [100.0, -32.0, 64.0]],
        material: "TEST/DOOR",
        flags: 0,
    };
    let models = vec![
        vec![TestBrush::cuboid([-512.0, -512.0, -32.0], [1024.0, 512.0, 0.0])],
        vec![TestBrush::cuboid([100.0, -32.0, 0.0], [108.0, 32.0, 64.0])],
    ];
    let sign = TestOverlay::square("DECALS/SIGN", vec![0, 2], [100.0, 0.0, 32.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 32.0);
    let map = TestMap { models, faces: faces(), model_faces: vec![(1, door)], overlays: vec![sign], ..TestMap::default() };
    let bsp = Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    let door = mesh::build(&bsp, mesh::Options { model: 1, ..mesh::Options::default() });
    assert_eq!(door.batches.len(), 1);
    assert!(door.batches.iter().all(|b| !b.overlay));
}
