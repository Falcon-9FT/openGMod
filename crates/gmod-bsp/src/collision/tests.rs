use std::sync::Arc;

use super::*;
use crate::surf;
use crate::testmap::{TestBrush, TestDisplacement, TestMap};

/// The player's box, from the feet.
const MINS: Vec3 = [-16.0, -16.0, 0.0];
const MAXS: Vec3 = [16.0, 16.0, 72.0];

fn load(map: &TestMap) -> (Bsp, Collision) {
    let bsp = Bsp::parse("test.bsp".into(), Arc::from(map.bytes())).unwrap();
    let collision = Collision::new(&bsp);
    (bsp, collision)
}

fn near(a: Vec3, b: Vec3) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-3)
}

fn material<'a>(bsp: &'a Bsp, trace: &Trace) -> Option<&'a str> {
    let info = bsp.texinfo.get(trace.texinfo?)?;
    bsp.materials.get(info.texdata as usize).map(String::as_str)
}

/// A floor with its top at 0, a brick wall from x = 256, a pool of water beside the floor, a
/// clip for players, a sky brush, and a ramp rising toward +x (z = x / 2 over its width).
fn room() -> TestMap {
    let world = vec![
        TestBrush::cuboid([-512.0, -512.0, -32.0], [512.0, 512.0, 0.0]),
        TestBrush::cuboid([256.0, -512.0, 0.0], [288.0, 512.0, 256.0]).material("BRICK/BRICKWALL001A", 0),
        TestBrush::cuboid([-768.0, -128.0, -128.0], [-512.0, 128.0, 0.0])
            .contents(contents::WATER)
            .material("NATURE/WATER_CANALS03", surf::WARP),
        TestBrush::cuboid([-128.0, -128.0, 0.0], [-96.0, 128.0, 128.0]).contents(contents::PLAYERCLIP).material("TOOLS/TOOLSPLAYERCLIP", 0),
        TestBrush::cuboid([-512.0, -512.0, 512.0], [512.0, 512.0, 544.0]).material("TOOLS/TOOLSSKYBOX", surf::SKY | surf::NODRAW),
        TestBrush::cuboid([0.0, -400.0, 0.0], [256.0, -300.0, 128.0]).cut([-0.5, 0.0, 1.0], 0.0),
        TestBrush::cuboid([-768.0, -128.0, -160.0], [-512.0, 128.0, -128.0]),
    ];
    TestMap { models: vec![world], ..TestMap::default() }
}

#[test]
fn reads_the_trees() {
    for old_leaves in [false, true] {
        let map = TestMap { old_leaves, ..room() };
        let bsp = Bsp::parse("test.bsp".into(), Arc::from(map.bytes())).unwrap();
        assert_eq!(bsp.lumps[crate::lumps::index::LEAFS].version, u32::from(!old_leaves));
        assert_eq!(bsp.brushes.len(), 7);
        assert_eq!(bsp.brushes[2].contents, contents::WATER);
        // six sides a box, a seventh for the ramp's slope
        assert_eq!(bsp.brush_sides.len(), 6 * 6 + 7);
        assert!(!bsp.nodes.is_empty() && bsp.leaves.len() > bsp.nodes.len());
        // every brush is in a leaf
        assert!((0..7u16).all(|b| bsp.leaf_brushes.contains(&b)));
        assert_eq!(bsp.leaves[0].num_leaf_brushes, 0);
        assert_eq!(bsp.models[0].head_node, 0);
    }
    // a child that points back up the tree would make a loop
    let mut bytes = room().bytes();
    let nodes = u32::from_le_bytes(bytes[8 + 5 * 16..8 + 5 * 16 + 4].try_into().unwrap()) as usize;
    bytes[nodes + 4..nodes + 8].copy_from_slice(&0i32.to_le_bytes());
    let err = Bsp::parse("x".into(), Arc::from(bytes)).unwrap_err();
    assert!(err.to_string().contains("node 0: child 0 out of range"), "{err}");
}

#[test]
fn masks_are_those_of_gmod() {
    // the numbers GMod's Lua has for them
    assert_eq!(mask::SOLID, 33570827);
    assert_eq!(mask::PLAYERSOLID, 33636363);
    assert_eq!(mask::NPCSOLID, 33701899);
    assert_eq!(mask::SOLID_BRUSHONLY, 16395);
    assert_eq!(mask::PLAYERSOLID_BRUSHONLY, 81931);
    assert_eq!(mask::WATER, 16432);
    assert_eq!(mask::OPAQUE, 16513);
    assert_eq!(mask::VISIBLE, 24705);
    assert_eq!(mask::SHOT, 1174421507);
}

#[test]
fn rays_stop_short_of_what_they_hit() {
    let (bsp, collision) = load(&room());
    let down = collision.trace_line([0.0, 0.0, 100.0], [0.0, 0.0, -100.0], mask::SOLID);
    assert!((down.fraction - (100.0 - EPSILON) / 200.0).abs() < 1e-6, "{}", down.fraction);
    assert!(near(down.end, [0.0, 0.0, EPSILON]), "{:?}", down.end);
    assert_eq!((down.normal, down.contents, down.model, down.displacement), ([0.0, 0.0, 1.0], contents::SOLID, Some(0), false));
    assert_eq!(material(&bsp, &down), Some("DEV/DEV_MEASUREGENERIC01"));
    assert!(down.hit() && !down.start_solid);

    let wall = collision.trace_line([0.0, 0.0, 64.0], [400.0, 0.0, 64.0], mask::SOLID);
    assert!(near(wall.end, [256.0 - EPSILON, 0.0, 64.0]), "{:?}", wall.end);
    assert_eq!(wall.normal, [-1.0, 0.0, 0.0]);
    assert_eq!(material(&bsp, &wall), Some("BRICK/BRICKWALL001A"));

    let sky = collision.trace_line([0.0, 0.0, 64.0], [0.0, 0.0, 1000.0], mask::SOLID);
    assert!(sky.surface_flags & surf::SKY != 0 && near(sky.end, [0.0, 0.0, 512.0 - EPSILON]));

    let free = collision.trace_line([0.0, 0.0, 64.0], [100.0, 50.0, 64.0], mask::SOLID);
    assert_eq!((free.fraction, free.end, free.normal, free.model), (1.0, [100.0, 50.0, 64.0], [0.0; 3], None));
    assert!(!free.hit());
}

#[test]
fn boxes_stop_where_their_sides_would_touch() {
    let (_, collision) = load(&room());
    let land = collision.trace([0.0, 0.0, 100.0], [0.0, 0.0, -100.0], MINS, MAXS, mask::PLAYERSOLID);
    assert!(near(land.end, [0.0, 0.0, EPSILON]), "{:?}", land.end);
    assert_eq!(land.normal, [0.0, 0.0, 1.0]);
    let wall = collision.trace([0.0, 0.0, 1.0], [400.0, 0.0, 1.0], MINS, MAXS, mask::PLAYERSOLID);
    assert!(near(wall.end, [256.0 - 16.0 - EPSILON, 0.0, 1.0]), "{:?}", wall.end);
    // a box whose middle is not where it is swept from: only the box counts
    let low = collision.trace([0.0, 0.0, 100.0], [0.0, 0.0, -100.0], [-1.0, -1.0, -50.0], [1.0, 1.0, -40.0], mask::SOLID);
    assert!(near(low.end, [0.0, 0.0, 50.0 + EPSILON]), "{:?}", low.end);
    // on the ramp, the box's high corner lands first: x 144 is 72 up the slope
    let ramp = collision.trace([128.0, -350.0, 200.0], [128.0, -350.0, 0.0], MINS, MAXS, mask::SOLID);
    let up = normalize([-0.5, 0.0, 1.0]);
    assert!(near(ramp.normal, up), "{:?}", ramp.normal);
    assert!(near(ramp.end, [128.0, -350.0, 72.0 + EPSILON / up[2]]), "{:?}", ramp.end);
    // and a ray reaches the slope under it
    let ray = collision.trace_line([128.0, -350.0, 200.0], [128.0, -350.0, 0.0], mask::SOLID);
    assert!(near(ray.end, [128.0, -350.0, 64.0 + EPSILON / up[2]]), "{:?}", ray.end);
    // past the low end of the ramp the floor is reached: the box catches on no edge that is not there
    let past = collision.trace([-17.0, -350.0, 50.0], [-17.0, -350.0, -10.0], MINS, MAXS, mask::SOLID);
    assert!(near(past.end, [-17.0, -350.0, EPSILON]), "{:?}", past.end);
}

#[test]
fn inside_and_out() {
    let (_, collision) = load(&room());
    let inside = collision.trace_line([0.0, 0.0, -10.0], [0.0, 0.0, -20.0], mask::SOLID);
    assert!(inside.start_solid && inside.all_solid);
    assert_eq!((inside.fraction, inside.end), (0.0, [0.0, 0.0, -10.0]));
    let out = collision.trace_line([0.0, 0.0, -10.0], [0.0, 0.0, 10.0], mask::SOLID);
    assert!(out.start_solid && !out.all_solid);
    assert_eq!(out.fraction, 1.0);
    assert!((out.fraction_left_solid - (10.0 - EPSILON) / 20.0).abs() < 1e-6, "{}", out.fraction_left_solid);
    // a box resting a little over the floor is not inside it
    let resting = collision.trace([0.0, 0.0, EPSILON], [10.0, 0.0, EPSILON], MINS, MAXS, mask::SOLID);
    assert!(!resting.start_solid && resting.fraction == 1.0);
}

#[test]
fn masks_choose_what_stops() {
    let (_, collision) = load(&room());
    // the clip stops players and nothing else
    let at = |mask| collision.trace([-200.0, 0.0, 1.0], [0.0, 0.0, 1.0], MINS, MAXS, mask);
    assert!(near(at(mask::PLAYERSOLID).end, [-128.0 - 16.0 - EPSILON, 0.0, 1.0]));
    assert_eq!(at(mask::PLAYERSOLID).contents, contents::PLAYERCLIP);
    assert_eq!(at(mask::SOLID).fraction, 1.0);
    // water is not solid, but a trace for water finds it
    assert!(near(collision.trace_line([-600.0, 0.0, 100.0], [-600.0, 0.0, -200.0], mask::SOLID).end, [-600.0, 0.0, -128.0 + EPSILON]));
    let surface = collision.trace_line([-600.0, 0.0, 100.0], [-600.0, 0.0, -200.0], mask::WATER);
    assert!(near(surface.end, [-600.0, 0.0, EPSILON]) && surface.contents == contents::WATER);
    assert_eq!(collision.point_contents([-600.0, 0.0, -10.0]), contents::WATER);
    assert_eq!(collision.point_contents([-600.0, 0.0, 10.0]), contents::EMPTY);
    assert_eq!(collision.point_contents([0.0, 0.0, -10.0]), contents::SOLID);
    assert_eq!(collision.point_contents([0.0, 0.0, 10.0]), contents::EMPTY);
}

/// A slope of 4 units a column over a displacement of power 2, from (600, 0, 0): z = (x - 600) / 8.
fn hill() -> TestMap {
    let heights = (0..25).map(|i| (i % 5) as f32 * 4.0).collect();
    TestMap {
        models: vec![vec![TestBrush::cuboid([-512.0, -512.0, -32.0], [512.0, 512.0, 0.0])]],
        displacements: vec![TestDisplacement { corner: [600.0, 0.0, 0.0], size: 128.0, power: 2, heights }],
        ..TestMap::default()
    }
}

#[test]
fn displacements_stop_what_comes_from_the_front() {
    let (bsp, collision) = load(&hill());
    let slope = normalize([-1.0, 0.0, 8.0]);
    let ray = collision.trace_line([664.0, 64.0, 100.0], [664.0, 64.0, -100.0], mask::SOLID);
    assert!(ray.displacement && ray.model == Some(0));
    assert!(near(ray.normal, slope), "{:?}", ray.normal);
    assert!(near(ray.end, [664.0, 64.0, 8.0 + EPSILON / slope[2]]), "{:?}", ray.end);
    assert_eq!(material(&bsp, &ray), Some("NATURE/BLENDGROUNDTOGRASS"));
    // from below it is not there
    let below = collision.trace_line([664.0, 64.0, -10.0], [664.0, 64.0, 100.0], mask::SOLID);
    assert_eq!(below.fraction, 1.0);
    assert!(!below.start_solid);
    // a box lands with its high side first: x 680 is 10 up
    let land = collision.trace([664.0, 64.0, 100.0], [664.0, 64.0, -100.0], MINS, MAXS, mask::SOLID);
    assert!(land.displacement);
    assert!(near(land.end, [664.0, 64.0, 10.0 + EPSILON / slope[2]]), "{:?}", land.end);
    // walking up it, the box meets the slope ahead (the ground under its front is 7 up)
    let walk = collision.trace([640.0, 64.0, 8.0], [700.0, 64.0, 8.0], MINS, MAXS, mask::SOLID);
    assert!(walk.fraction < 1.0 && near(walk.normal, slope), "{walk:?}");
    // past its side there is no displacement
    let beside = collision.trace_line([664.0, 140.0, 100.0], [664.0, 140.0, -100.0], mask::SOLID);
    assert_eq!(beside.fraction, 1.0);
}

#[test]
fn brush_entities_stand_where_the_map_puts_them() {
    let small = TestBrush::cuboid([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0]);
    let long = TestBrush::cuboid([0.0, -8.0, 0.0], [64.0, 8.0, 16.0]);
    let map = TestMap {
        models: vec![
            vec![TestBrush::cuboid([-512.0, -512.0, -32.0], [512.0, 512.0, 0.0])],
            vec![small.clone()],
            vec![long],
            vec![small.clone()],
            vec![small],
        ],
        entities: [
            "{\n\"classname\" \"func_brush\"\n\"model\" \"*1\"\n\"origin\" \"0 300 0\"\n}\n",
            "{\n\"classname\" \"func_door\"\n\"model\" \"*2\"\n\"origin\" \"200 -300 0\"\n\"angles\" \"0 90 0\"\n}\n",
            "{\n\"classname\" \"trigger_once\"\n\"model\" \"*3\"\n\"origin\" \"-200 0 0\"\n}\n",
            "{\n\"classname\" \"func_brush\"\n\"model\" \"*4\"\n\"origin\" \"-300 0 0\"\n\"Solidity\" \"1\"\n}\n",
        ]
        .concat(),
        ..TestMap::default()
    };
    let (_, collision) = load(&map);
    assert_eq!(collision.placements.iter().map(|p| p.model).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(collision.placements[1].entity, Some(1), "after worldspawn");
    let down = |x: f32, y: f32| collision.trace_line([x, y, 100.0], [x, y, -100.0], mask::SOLID);
    let moved = down(0.0, 300.0);
    assert!(near(moved.end, [0.0, 300.0, 32.0 + EPSILON]) && moved.model == Some(1), "{moved:?}");
    // turned a quarter: the long side lies along +y now
    let turned = down(200.0, -250.0);
    assert!(near(turned.end, [200.0, -250.0, 16.0 + EPSILON]) && turned.model == Some(2), "{turned:?}");
    assert_eq!(down(250.0, -300.0).model, Some(0));
    // the normal turns along: the end of the long side faces +y
    let side = collision.trace_line([200.0, -200.0, 8.0], [200.0, -260.0, 8.0], mask::SOLID);
    assert!(near(side.normal, [0.0, 1.0, 0.0]) && near(side.end, [200.0, -236.0 + EPSILON, 8.0]), "{side:?}");
    // a box that is turned with it holds the box
    let boxed = collision.trace([180.0, -250.0, 8.0], [230.0, -250.0, 8.0], [-4.0; 3], [4.0; 3], mask::SOLID);
    assert!(boxed.fraction < 1.0 && boxed.end[0] <= 192.0 - 4.0 + 1e-3, "{boxed:?}");
    // a trigger and a brush that is never solid are not there
    assert_eq!(down(-200.0, 0.0).model, Some(0));
    assert_eq!(down(-300.0, 0.0).model, Some(0));
    // one model alone
    let alone =
        collision.trace_placed(&collision.placements[1], [0.0, 300.0, 100.0], [0.0, 300.0, -100.0], [0.0; 3], [0.0; 3], mask::SOLID);
    assert_eq!(alone.model, Some(1));
}

/// A small random generator, the same every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

#[test]
fn the_tree_finds_what_every_brush_would() {
    let mut random = Random(7);
    let mut brushes = Vec::new();
    for _ in 0..60 {
        let mins = [random.range(-500.0, 400.0), random.range(-500.0, 400.0), random.range(-500.0, 400.0)];
        let maxs = mins.map(|m| m + 8.0 + 120.0 * random.next());
        let mut brush = TestBrush::cuboid(mins, maxs);
        if random.next() < 0.3 {
            // a slope through the middle
            let n = [random.range(-1.0, 1.0), random.range(-1.0, 1.0), 1.0];
            let middle = [(mins[0] + maxs[0]) / 2.0, (mins[1] + maxs[1]) / 2.0, (mins[2] + maxs[2]) / 2.0];
            brush = brush.cut(n, n[0] * middle[0] + n[1] * middle[1] + n[2] * middle[2]);
        }
        brushes.push(brush);
    }
    let (_, collision) = load(&TestMap { models: vec![brushes], ..TestMap::default() });
    let mut hits = 0;
    for i in 0..3000 {
        let start = [random.range(-600.0, 600.0), random.range(-600.0, 600.0), random.range(-600.0, 600.0)];
        let end = if i % 3 == 0 { start } else { [random.range(-600.0, 600.0), random.range(-600.0, 600.0), random.range(-600.0, 600.0)] };
        let (mins, maxs) = if i % 2 == 0 { ([0.0; 3], [0.0; 3]) } else { (MINS, MAXS) };
        let tree = collision.trace(start, end, mins, maxs, mask::SOLID);
        // every brush, without the tree
        let offset = scale(add(mins, maxs), 0.5);
        let extents = scale(sub(maxs, mins), 0.5);
        let mut sweep = Sweep {
            start: add(start, offset),
            end: add(end, offset),
            extents,
            axes: None,
            point: extents == [0.0; 3],
            mask: mask::SOLID,
            fraction: 1.0,
            hit: None,
            start_solid: false,
            all_solid: false,
            left_solid: 0.0,
        };
        for brush in &collision.brushes {
            let sides = &collision.sides[brush.first_side as usize..(brush.first_side + brush.sides) as usize];
            sweep.clip(sides.iter().copied(), brush.contents, false, false);
        }
        assert_eq!((tree.fraction, tree.start_solid, tree.all_solid), (sweep.fraction, sweep.start_solid, sweep.all_solid), "sweep {i}");
        if tree.fraction < 1.0 {
            hits += 1;
        }
    }
    assert!(hits > 500, "{hits}");
}

#[test]
fn a_map_without_trees_has_nothing_solid() {
    let bsp = Bsp::parse("test.bsp".into(), Arc::from(crate::testmap::bytes())).unwrap();
    let collision = Collision::new(&bsp);
    assert!(collision.is_empty());
    assert_eq!(collision.trace_line([0.0; 3], [0.0, 0.0, -100.0], mask::ALL).fraction, 1.0);
    assert_eq!(collision.point_contents([0.0; 3]), contents::EMPTY);
}

#[test]
fn hulls_are_solids() {
    let tetrahedron = [[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 16.0, 0.0], [0.0, 0.0, 16.0]];
    let triangles = [[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2], [0, 1, 99]];
    let hull = Hull::new(&tetrahedron, &triangles).unwrap();
    assert_eq!(hull.faces.len(), 4, "the triangle with a corner that is not there is passed over");
    assert!(hull.faces.iter().all(|&(n, d)| dot(n, [4.0, 4.0, 4.0]) < d), "facing out");
    assert_eq!((hull.mins, hull.maxs), ([0.0; 3], [16.0; 3]));
    let flat = [[0.0, 0.0, 0.0], [16.0, 0.0, 0.0], [0.0, 16.0, 0.0], [16.0, 16.0, 0.0]];
    assert!(Hull::new(&flat, &[[0, 1, 2], [1, 3, 2]]).is_none());
    assert!(Hull::new(&tetrahedron[..3], &[[0, 1, 2]]).is_none());
}

#[test]
fn static_props_collide_by_their_hulls() {
    let floor = TestMap { models: vec![vec![TestBrush::cuboid([-1024.0, -1024.0, -32.0], [1024.0, 1024.0, 0.0])]], ..TestMap::default() };
    let (_, mut collision) = load(&floor);
    let crate_shape = collision.add_shape(vec![Hull::cuboid([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0])]);
    collision.add_static(7, crate_shape, [200.0, 0.0, 0.0], [0.0; 3]);
    collision.add_static(8, crate_shape, [400.0, 0.0, 0.0], [0.0, 45.0, 0.0]);
    collision.add_static(9, crate_shape + 1, [600.0, 0.0, 0.0], [0.0; 3]);

    let top = collision.trace_line([200.0, 0.0, 100.0], [200.0, 0.0, -100.0], mask::SOLID);
    assert!(near(top.end, [200.0, 0.0, 32.0 + EPSILON]) && top.normal == [0.0, 0.0, 1.0], "{top:?}");
    assert_eq!((top.static_prop, top.model, top.contents, top.texinfo), (Some(7), None, contents::SOLID, None));
    let side = collision.trace([100.0, 0.0, 1.0], [300.0, 0.0, 1.0], MINS, MAXS, mask::PLAYERSOLID);
    assert!(near(side.end, [200.0 - 32.0 - EPSILON, 0.0, 1.0]), "{side:?}");

    // turned a quarter of a right angle, a corner points at the box: the box's own face meets
    // it, and the box is not grown by being turned
    let corner = 400.0 - 16.0 * 2f32.sqrt();
    let turned = collision.trace([300.0, 0.0, 1.0], [500.0, 0.0, 1.0], MINS, MAXS, mask::PLAYERSOLID);
    assert!(near(turned.end, [corner - 16.0 - EPSILON, 0.0, 1.0]) && near(turned.normal, [-1.0, 0.0, 0.0]), "{turned:?}");
    assert_eq!(turned.static_prop, Some(8));
    let beside = collision.trace([300.0, 40.0, 1.0], [500.0, 40.0, 1.0], MINS, MAXS, mask::PLAYERSOLID);
    assert_eq!(beside.fraction, 1.0, "{beside:?}");
    // a ray meets the corner, stopping short of one of the faces there
    let ray = collision.trace_line([300.0, 0.0, 10.0], [500.0, 0.0, 10.0], mask::SOLID);
    assert!(near(ray.end, [corner - EPSILON * 2f32.sqrt(), 0.0, 10.0]), "{ray:?}");
    assert!((ray.normal[0] + 0.5f32.sqrt()).abs() < 1e-4 && (ray.normal[1].abs() - 0.5f32.sqrt()).abs() < 1e-4, "{ray:?}");

    // not water, and a shape that was not added is nothing
    assert_eq!(collision.trace_line([200.0, 0.0, 100.0], [200.0, 0.0, 10.0], mask::WATER).fraction, 1.0);
    assert_eq!(collision.trace_line([600.0, 0.0, 100.0], [600.0, 0.0, 10.0], mask::SOLID).fraction, 1.0);
    assert_eq!(collision.point_contents([200.0, 0.0, 10.0]), contents::EMPTY);
}

#[test]
fn a_hull_stops_what_the_same_brush_stops() {
    let mut random = Random(11);
    let boxes: Vec<(Vec3, Vec3)> = (0..30)
        .map(|_| {
            let mins = [random.range(-400.0, 300.0), random.range(-400.0, 300.0), random.range(-400.0, 300.0)];
            (mins, mins.map(|m| m + 8.0 + 100.0 * random.next()))
        })
        .collect();
    let brushes = boxes.iter().map(|&(mins, maxs)| TestBrush::cuboid(mins, maxs)).collect();
    let (_, as_brushes) = load(&TestMap { models: vec![brushes], ..TestMap::default() });
    let (_, mut as_hulls) = load(&TestMap { models: vec![Vec::new()], ..TestMap::default() });
    for (i, &(mins, maxs)) in boxes.iter().enumerate() {
        // the hull in its own space, moved back by its prop's origin
        let middle = scale(add(mins, maxs), 0.5);
        let shape = as_hulls.add_shape(vec![Hull::cuboid(sub(mins, middle), sub(maxs, middle))]);
        as_hulls.add_static(i, shape, middle, [0.0; 3]);
    }
    for i in 0..2000 {
        let start = [random.range(-500.0, 500.0), random.range(-500.0, 500.0), random.range(-500.0, 500.0)];
        let end = [random.range(-500.0, 500.0), random.range(-500.0, 500.0), random.range(-500.0, 500.0)];
        let (mins, maxs) = if i % 2 == 0 { ([0.0; 3], [0.0; 3]) } else { (MINS, MAXS) };
        let (a, b) = (as_brushes.trace(start, end, mins, maxs, mask::SOLID), as_hulls.trace(start, end, mins, maxs, mask::SOLID));
        assert!((a.fraction - b.fraction).abs() < 1e-5, "sweep {i}: {a:?} {b:?}");
        assert_eq!((a.start_solid, a.all_solid), (b.start_solid, b.all_solid), "sweep {i}");
    }
}

#[test]
fn shapes_for_physics() {
    let (_, mut collision) = load(&room());
    let crate_shape = collision.add_shape(vec![Hull::cuboid([-16.0, -16.0, 0.0], [16.0, 16.0, 32.0])]);
    collision.add_static(0, crate_shape, [100.0, 0.0, 0.0], [0.0, 90.0, 0.0]);
    let shapes = collision.solid_shapes(mask::SOLID_BRUSHONLY);
    // the floor, the wall, the sky, the ramp, the brush under the water - not the water nor the
    // clip for players - and the crate
    assert_eq!(shapes.hulls.len(), 6, "{:?}", shapes.hulls.iter().map(Vec::len).collect::<Vec<_>>());
    let has = |corner: Vec3| shapes.hulls.iter().any(|h| h.iter().any(|&p| near(p, corner)));
    let floor = shapes.hulls.iter().find(|h| h.iter().any(|&p| near(p, [-512.0, -512.0, -32.0]))).unwrap();
    assert_eq!(floor.len(), 8);
    assert!(floor.iter().all(|p| p[2] == 0.0 || p[2] == -32.0));
    // the ramp is cut: its high corners stand at x / 2
    assert!(has([256.0, -400.0, 128.0]) && has([0.0, -400.0, 0.0]) && !has([0.0, -400.0, 128.0]));
    assert!(has([116.0, 16.0, 32.0]), "the crate where it stands, turned");
    assert!(shapes.triangles.is_empty());

    let (_, collision) = load(&hill());
    let shapes = collision.solid_shapes(mask::SOLID_BRUSHONLY);
    assert!(!shapes.triangles.is_empty() && shapes.vertices.len() == shapes.triangles.len() * 3);
}
