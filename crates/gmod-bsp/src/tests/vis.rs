//! The visibility lump: clusters, and what each sees and hears.

use super::*;

/// Rooms along x, each `width` wide and 128 deep and high, with walls 16 thick between them,
/// each room a cluster; neighbours see each other.
fn rooms(count: usize, width: f32) -> Bsp {
    let step = width + 16.0;
    let length = step * count as f32;
    let mut world = vec![
        TestBrush::cuboid([-16.0, -16.0, -16.0], [length, 144.0, 0.0]),
        TestBrush::cuboid([-16.0, -16.0, 128.0], [length, 144.0, 144.0]),
        TestBrush::cuboid([-16.0, -16.0, 0.0], [length, 0.0, 128.0]),
        TestBrush::cuboid([-16.0, 128.0, 0.0], [length, 144.0, 128.0]),
        TestBrush::cuboid([-16.0, 0.0, 0.0], [0.0, 128.0, 128.0]),
    ];
    let mut clusters = Vec::new();
    for i in 0..count {
        let lo = step * i as f32;
        clusters.push(([lo, 0.0, 0.0], [lo + width, 128.0, 128.0]));
        world.push(TestBrush::cuboid([lo + width, 0.0, 0.0], [lo + step, 128.0, 128.0]));
    }
    let sees = (1..count).map(|i| (i - 1, i)).collect();
    let map = TestMap { models: vec![world], clusters, sees, ..TestMap::default() };
    Bsp::parse("rooms.bsp".into(), Arc::from(map.bytes())).unwrap()
}

#[test]
fn clusters_see_and_hear_as_the_compiler_wrote() {
    let bsp = rooms(3, 128.0);
    let vis = Visibility::new(&bsp).unwrap();
    assert_eq!(vis.cluster_count(), 3);
    // the middle of each room is in its cluster; a wall and outside the world are in none
    assert_eq!(vis.cluster_at([64.0, 64.0, 64.0]), Some(0));
    assert_eq!(vis.cluster_at([208.0, 64.0, 64.0]), Some(1));
    assert_eq!(vis.cluster_at([352.0, 64.0, 64.0]), Some(2));
    assert_eq!(vis.cluster_at([136.0, 64.0, 64.0]), None);
    assert_eq!(vis.cluster_at([64.0, 64.0, 1000.0]), None);
    // the first room sees the second, not the third; it hears the third through the second
    let (pvs, pas) = (vis.pvs(0), vis.pas(0));
    assert!(row_has(pvs, 0) && row_has(pvs, 1) && !row_has(pvs, 2));
    assert!(row_has(pas, 0) && row_has(pas, 1) && row_has(pas, 2));
    assert!(row_has(vis.pvs(2), 1) && !row_has(vis.pvs(2), 0));
    // a box across the first wall is in the first two clusters; one in a room, in its own
    let mut out = Vec::new();
    assert!(vis.clusters_in_box([100.0, 32.0, 0.0], [160.0, 96.0, 72.0], &mut out));
    out.sort();
    assert_eq!(out, [0, 1]);
    assert!(vis.clusters_in_box([300.0, 32.0, 0.0], [320.0, 96.0, 72.0], &mut out));
    assert_eq!(out, [2]);
}

#[test]
fn long_rows_unpack_their_runs_of_zeros() {
    // 40 clusters: rows of five bytes, mostly zeros
    let bsp = rooms(40, 32.0);
    let vis = Visibility::new(&bsp).unwrap();
    assert_eq!(vis.cluster_count(), 40);
    let row = vis.pvs(20);
    assert_eq!(row.len(), 5);
    let seen: Vec<usize> = (0..40).filter(|&c| row_has(row, c)).collect();
    assert_eq!(seen, [19, 20, 21]);
    let heard: Vec<usize> = (0..40).filter(|&c| row_has(vis.pas(39), c)).collect();
    assert_eq!(heard, [37, 38, 39]);
    assert_eq!(vis.cluster_at([39.0 * 48.0 + 16.0, 64.0, 64.0]), Some(39));
}

#[test]
fn a_map_without_the_lump_has_no_visibility() {
    let map = TestMap { models: vec![vec![TestBrush::cuboid([0.0; 3], [64.0; 3])]], ..TestMap::default() };
    let bsp = Bsp::parse("t.bsp".into(), Arc::from(map.bytes())).unwrap();
    assert!(Visibility::new(&bsp).is_none());
}

#[test]
fn shut_area_portals_part_the_areas() {
    // three rooms in a row, each an area, a portal of key 1 between the first two and of key 2
    // between the last two
    let mut world = vec![TestBrush::cuboid([-16.0, -16.0, -16.0], [432.0, 144.0, 0.0])];
    world.push(TestBrush::cuboid([128.0, 0.0, 0.0], [144.0, 128.0, 128.0]));
    world.push(TestBrush::cuboid([272.0, 0.0, 0.0], [288.0, 128.0, 128.0]));
    let map = TestMap {
        models: vec![world],
        clusters: vec![
            ([0.0, 0.0, 0.0], [128.0, 128.0, 128.0]),
            ([144.0, 0.0, 0.0], [272.0, 128.0, 128.0]),
            ([288.0, 0.0, 0.0], [416.0, 128.0, 128.0]),
        ],
        sees: vec![(0, 1), (1, 2), (0, 2)],
        areas: vec![
            ([0.0, 0.0, 0.0], [128.0, 128.0, 128.0], 1),
            ([144.0, 0.0, 0.0], [272.0, 128.0, 128.0], 2),
            ([288.0, 0.0, 0.0], [416.0, 128.0, 128.0], 3),
        ],
        area_portals: vec![(1, 2, 1), (2, 3, 2)],
        ..TestMap::default()
    };
    let bsp = Bsp::parse("areas.bsp".into(), Arc::from(map.bytes())).unwrap();
    assert_eq!((bsp.areas.len(), bsp.area_portals.len()), (4, 4));
    let vis = Visibility::new(&bsp).unwrap();
    assert_eq!(
        (vis.area_at([64.0, 64.0, 64.0]), vis.area_at([352.0, 64.0, 64.0]), vis.area_at([64.0, 64.0, 500.0])),
        (Some(1), Some(3), None)
    );
    let mut areas = Vec::new();
    vis.areas_in_box([100.0, 32.0, 0.0], [160.0, 96.0, 72.0], &mut areas);
    areas.sort();
    assert_eq!(areas, [1, 2]);
    // all open: all joined; the second shut: the last room is apart; the first: only the first
    let reached = |open: &dyn Fn(u16) -> bool| -> Vec<u16> {
        vis.connected_areas(1, open).iter().enumerate().filter(|(_, r)| **r).map(|(a, _)| a as u16).collect()
    };
    assert_eq!(reached(&|_| true), [1, 2, 3]);
    assert_eq!(reached(&|key| key != 2), [1, 2]);
    assert_eq!(reached(&|key| key != 1), [1]);
}
