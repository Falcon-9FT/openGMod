use std::sync::Arc;

use gmod_bsp::collision::EPSILON;
use gmod_bsp::testmap::{TestBrush, TestDisplacement, TestMap};
use gmod_bsp::{Bsp, Collision};

use super::*;

/// A tick of the server: 66⅔ a second.
const TICK: f32 = 0.015;

fn world(mut brushes: Vec<TestBrush>, displacements: Vec<TestDisplacement>) -> Collision {
    // a floor with its top at 0
    brushes.insert(0, TestBrush::cuboid([-4096.0, -4096.0, -64.0], [4096.0, 4096.0, 0.0]));
    let map = TestMap { models: vec![brushes], displacements, ..TestMap::default() };
    Collision::new(&Bsp::parse("test.bsp".into(), Arc::from(map.bytes())).unwrap())
}

fn forward(buttons: u32) -> Command {
    Command { forward_move: FULL_MOVE, buttons, ..Command::default() }
}

/// Runs ticks for `seconds`, and returns the highest the feet got.
fn run(player: &mut Player, world: &Collision, command: &Command, seconds: f32) -> f32 {
    let mut highest = player.origin[2];
    for _ in 0..(seconds / TICK).round() as usize {
        player.tick(world, &Settings::default(), command, TICK);
        highest = highest.max(player.origin[2]);
    }
    highest
}

fn standing(world: &Collision, origin: Vec3) -> Player {
    let mut player = Player::spawn(world, &Settings::default(), origin).unwrap();
    run(&mut player, world, &Command::default(), 0.1);
    assert!(player.on_ground, "{player:?}");
    player
}

fn horizontal(v: Vec3) -> f32 {
    v[0].hypot(v[1])
}

#[test]
fn spawns_above_a_floor_it_would_touch() {
    let world = world(Vec::new(), Vec::new());
    let player = Player::spawn(&world, &Settings::default(), [0.0; 3]).unwrap();
    // touching the floor at 0, room at 1, and then down on the floor
    assert_eq!(player.origin, [0.0, 0.0, EPSILON]);
    assert!(player.on_ground && player.water_level == 0);
    assert_eq!(player.eye(), [0.0, 0.0, 64.0 + EPSILON]);
    // inside a wall there is no room
    let walled = self::world(vec![TestBrush::cuboid([-100.0; 3], [100.0, 100.0, 300.0])], Vec::new());
    assert!(Player::spawn(&walled, &Settings::default(), [0.0; 3]).is_none());
}

#[test]
fn gets_out_of_what_it_is_stuck_in() {
    let world = world(vec![TestBrush::cuboid([20.0, -100.0, 0.0], [100.0, 100.0, 100.0])], Vec::new());
    // sunk into the floor; into a wall, touching the floor
    for origin in [[0.0, 0.0, -0.5], [10.0, 0.0, 0.0]] {
        let mut player = Player { origin, ..standing(&world, [-200.0, 0.0, 0.0]) };
        run(&mut player, &world, &Command::default(), 0.1);
        assert!(fits(&world, player.origin, Settings::default().hull) && player.on_ground, "{player:?}");
        assert!(player.origin[0] < 4.0 - EPSILON && player.origin[2].abs() < 1.0, "{:?}", player.origin);
    }
}

#[test]
fn falls_as_gravity_says_and_lands() {
    let world = world(Vec::new(), Vec::new());
    let mut player = Player::spawn(&world, &Settings::default(), [0.0, 0.0, 100.0]).unwrap();
    assert!(!player.on_ground);
    run(&mut player, &world, &Command::default(), 0.3);
    // 600 units a second, a second: 27 units in 0.3 s, whatever the tick
    assert!((player.origin[2] - 73.0).abs() < 0.01, "{:?}", player.origin);
    assert!((player.velocity[2] + 180.0).abs() < 0.01, "{:?}", player.velocity);
    run(&mut player, &world, &Command::default(), 0.5);
    assert!(player.on_ground && player.velocity == [0.0; 3]);
    assert!((player.origin[2] - EPSILON).abs() < 1e-3, "{:?}", player.origin);
}

#[test]
fn walks_and_runs_at_their_speeds() {
    let world = world(Vec::new(), Vec::new());
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &forward(0), 0.5);
    assert!((horizontal(player.velocity) - 200.0).abs() < 0.01, "{:?}", player.velocity);
    assert!(player.velocity[1].abs() < 1e-3 && player.velocity[0] > 0.0, "yaw 0 is +x");
    run(&mut player, &world, &forward(buttons::SPEED), 0.5);
    assert!((horizontal(player.velocity) - 400.0).abs() < 0.01, "{:?}", player.velocity);
    // forward and sideways at once is no faster
    let diagonal = Command { forward_move: FULL_MOVE, side_move: FULL_MOVE, angles: [0.0, 90.0, 0.0], ..Command::default() };
    run(&mut player, &world, &diagonal, 1.0);
    assert!((horizontal(player.velocity) - 200.0).abs() < 0.01, "{:?}", player.velocity);
    // looking along +y, the right is +x
    assert!(player.velocity[0] > 100.0 && player.velocity[1] > 100.0, "{:?}", player.velocity);
    assert!(player.on_ground && player.origin[2] == EPSILON);
}

#[test]
fn friction_stops_a_player() {
    let world = world(Vec::new(), Vec::new());
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &forward(0), 0.5);
    let from = player.origin[0];
    run(&mut player, &world, &Command::default(), 0.3);
    assert!(player.velocity[0] > 0.0 && player.velocity[0] < 30.0, "{:?}", player.velocity);
    run(&mut player, &world, &Command::default(), 0.3);
    assert_eq!(player.velocity, [0.0; 3]);
    // from 200 down to 10 as speed × 8 a second (190 / 8 units), then 80 a second
    let slid = player.origin[0] - from;
    assert!((20.0..27.0).contains(&slid), "{slid}");
}

#[test]
fn walls_stop_and_the_player_slides_along_them() {
    let world = world(vec![TestBrush::cuboid([200.0, -1000.0, 0.0], [232.0, 1000.0, 200.0])], Vec::new());
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &forward(0), 2.0);
    assert!((player.origin[0] - (200.0 - 16.0 - EPSILON)).abs() < 1e-3, "{:?}", player.origin);
    assert!(player.origin[1].abs() < 1e-3);
    // at 45° to the wall: the part along it goes on
    let slanted = Command { angles: [0.0, 45.0, 0.0], ..forward(0) };
    run(&mut player, &world, &slanted, 1.0);
    assert!((player.origin[0] - (200.0 - 16.0 - EPSILON)).abs() < 1e-3, "{:?}", player.origin);
    assert!(player.origin[1] > 100.0, "{:?}", player.origin);
}

#[test]
fn steps_up_to_the_step_size() {
    let low = world(vec![TestBrush::cuboid([100.0, -1000.0, 0.0], [1000.0, 1000.0, 16.0])], Vec::new());
    let mut player = standing(&low, [0.0; 3]);
    run(&mut player, &low, &forward(0), 1.5);
    assert!(player.origin[0] > 150.0 && player.on_ground, "{player:?}");
    assert!((player.origin[2] - (16.0 + EPSILON)).abs() < 0.1, "{:?}", player.origin);
    let high = world(vec![TestBrush::cuboid([100.0, -1000.0, 0.0], [1000.0, 1000.0, 24.0])], Vec::new());
    let mut player = standing(&high, [0.0; 3]);
    run(&mut player, &high, &forward(0), 1.5);
    assert!(player.origin[0] < 100.0 - 16.0 && player.origin[2] < 2.0, "{:?}", player.origin);
    // and down again, without leaving the ground
    let mut player = standing(&low, [300.0, 0.0, 16.0]);
    let back = Command { angles: [0.0, 180.0, 0.0], ..forward(0) };
    for _ in 0..100 {
        player.tick(&low, &Settings::default(), &back, TICK);
        assert!(player.on_ground, "{player:?}");
    }
    assert!(player.origin[0] < 50.0 && player.origin[2] < 1.0, "{:?}", player.origin);
}

/// A ramp from x = 100, rising `rise` a unit along x.
fn ramp(rise: f32) -> TestBrush {
    TestBrush::cuboid([100.0, -500.0, 0.0], [600.0, 500.0, 500.0 * rise]).cut([-rise, 0.0, 1.0], -100.0 * rise)
}

#[test]
fn walks_up_gentle_slopes_and_slides_off_steep_ones() {
    // 30°: walked up
    let gentle = world(vec![ramp(30f32.to_radians().tan())], Vec::new());
    let mut player = standing(&gentle, [0.0; 3]);
    run(&mut player, &gentle, &forward(0), 1.5);
    assert!(player.on_ground && player.origin[0] > 200.0 && player.origin[2] > 40.0, "{player:?}");
    // 60°: no ground to stand on, the player slides down and off
    let steep = world(vec![ramp(60f32.to_radians().tan())], Vec::new());
    let mut player = Player::spawn(&steep, &Settings::default(), [200.0, 0.0, 300.0]).unwrap();
    let mut ever_on_ground = false;
    for _ in 0..200 {
        player.tick(&steep, &Settings::default(), &Command::default(), TICK);
        ever_on_ground |= player.on_ground && player.origin[2] > 1.0;
    }
    assert!(!ever_on_ground);
    assert!(player.on_ground && player.origin[0] < 100.0 && player.origin[2] < 1.0, "{player:?}");
}

#[test]
fn jumps_as_high_as_its_power_and_only_when_pressed() {
    let world = world(Vec::new(), Vec::new());
    let mut player = standing(&world, [0.0; 3]);
    let jump = Command { buttons: buttons::JUMP, ..Command::default() };
    let highest = run(&mut player, &world, &jump, 0.8);
    // 200² / (2 × 600) above the start, and down again in 2 × 200 / 600 seconds
    assert!((highest - EPSILON - 200.0 * 200.0 / 1200.0).abs() < 0.01, "{highest}");
    assert!(player.on_ground);
    // still held: no further jump
    let again = run(&mut player, &world, &jump, 1.0);
    assert!(again < 1.0 && player.on_ground, "{again}");
    run(&mut player, &world, &Command::default(), TICK);
    assert!(run(&mut player, &world, &jump, 0.4) > 30.0);
}

#[test]
fn ducks_under_what_a_standing_player_cannot_pass() {
    // a gap 50 high from x = 100 on
    let world = world(vec![TestBrush::cuboid([100.0, -500.0, 50.0], [400.0, 500.0, 200.0])], Vec::new());
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &forward(0), 1.0);
    assert!(player.origin[0] < 100.0, "{:?}", player.origin);
    let duck = Command { buttons: buttons::DUCK, ..Command::default() };
    run(&mut player, &world, &duck, 0.15);
    // half way down after half the time, the box still the big one
    assert!(!player.ducked && (player.view_height - 46.0).abs() < 1.0, "{player:?}");
    run(&mut player, &world, &duck, 0.2);
    assert!(player.ducked && player.view_height == 28.0);
    assert_eq!(player.hull(&Settings::default()).1[2], 36.0);
    run(&mut player, &world, &forward(buttons::DUCK), 1.0);
    assert!(player.origin[0] > 100.0, "{:?}", player.origin);
    // ducking, a third of the walking speed
    assert!((horizontal(player.velocity) - 60.0).abs() < 0.01, "{:?}", player.velocity);
    // no room to stand up under it
    run(&mut player, &world, &Command::default(), 1.0);
    assert!(player.ducked && player.view_height == 28.0);
    // and out from under it, up again
    let out = Command { angles: [0.0, 180.0, 0.0], ..forward(buttons::DUCK) };
    run(&mut player, &world, &out, 3.0);
    run(&mut player, &world, &Command::default(), 0.5);
    assert!(!player.ducked && player.view_height == 64.0, "{player:?}");
}

#[test]
fn ducking_in_the_air_pulls_the_feet_up() {
    let world = world(Vec::new(), Vec::new());
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &Command { buttons: buttons::JUMP, ..Command::default() }, 0.15);
    let (feet, eye) = (player.origin[2], player.eye()[2]);
    player.tick(&world, &Settings::default(), &Command { buttons: buttons::JUMP | buttons::DUCK, ..Command::default() }, TICK);
    assert!(player.ducked);
    // the feet rose by the difference of the boxes, plus the tick's own rise
    assert!(player.origin[2] - feet > 36.0 && player.origin[2] - feet < 40.0, "{feet} {:?}", player.origin);
    assert!((player.eye()[2] - eye).abs() < 4.0);
    // landed ducked, the player stands up again
    run(&mut player, &world, &Command::default(), 2.0);
    assert!(!player.ducked && player.on_ground);
}

#[test]
fn swims_in_water() {
    // a pool 256 deep beside the floor
    let pool = vec![
        TestBrush::cuboid([5000.0, -500.0, -256.0], [6000.0, 500.0, 0.0]).contents(contents::WATER),
        TestBrush::cuboid([5000.0, -500.0, -300.0], [6000.0, 500.0, -256.0]),
    ];
    let world = world(pool, Vec::new());
    let mut player = Player::spawn(&world, &Settings::default(), [5500.0, 0.0, -200.0]).unwrap();
    assert_eq!(player.water_level, 3);
    run(&mut player, &world, &Command::default(), 1.0);
    // sinking, not falling
    assert!(player.origin[2] < -200.0 && player.origin[2] > -260.0, "{:?}", player.origin);
    assert!(player.velocity[2] > -60.0);
    let from = player.origin[2];
    run(&mut player, &world, &Command { buttons: buttons::JUMP, ..Command::default() }, 1.0);
    assert!(player.origin[2] > from + 30.0, "{from} {:?}", player.origin);
    // looking down and swimming forward goes down
    let from = player.origin[2];
    run(&mut player, &world, &Command { angles: [60.0, 0.0, 0.0], ..forward(0) }, 0.5);
    assert!(player.origin[2] < from - 20.0, "{from} {:?}", player.origin);
    // out of the water the player falls again
    let mut dry = Player::spawn(&world, &Settings::default(), [0.0, 0.0, 100.0]).unwrap();
    run(&mut dry, &world, &Command::default(), 0.3);
    assert_eq!(dry.water_level, 0);
    assert!(dry.origin[2] < 80.0);
}

#[test]
fn walks_up_a_displacement() {
    // from x = 600: 4 up every 32 along x
    let heights = (0..25).map(|i| (i % 5) as f32 * 4.0).collect();
    let hill = TestDisplacement { corner: [600.0, 0.0, 0.0], size: 128.0, power: 2, heights };
    let world = world(Vec::new(), vec![hill]);
    let mut player = standing(&world, [540.0, 64.0, 0.0]);
    let mut stood = 0;
    for _ in 0..200 {
        player.tick(&world, &Settings::default(), &forward(0), TICK);
        let [x, _, z] = player.origin;
        if (620.0..700.0).contains(&x) {
            // the front of the box is on the slope: 16 ahead of the feet
            let ground = (x + 16.0 - 600.0) / 8.0;
            assert!(z > ground && z < ground + 0.1 && player.on_ground, "{:?}", player.origin);
            stood += 1;
        }
    }
    assert!(stood > 20, "{stood}");
}

/// A block from x = 100, 256 high, with a ladder in front of it: solid, or only a ladder.
fn ladder_wall(solid: bool) -> Collision {
    let contents = if solid { contents::LADDER | contents::SOLID | contents::PLAYERCLIP } else { contents::LADDER };
    world(
        vec![
            TestBrush::cuboid([100.0, -200.0, 0.0], [600.0, 200.0, 256.0]),
            TestBrush::cuboid([96.0, -32.0, 0.0], [100.0, 32.0, 256.0]).contents(contents).material("TOOLS/TOOLSINVISIBLELADDER", 0),
        ],
        Vec::new(),
    )
}

#[test]
fn climbs_ladders() {
    for solid in [true, false] {
        let world = ladder_wall(solid);
        let mut player = standing(&world, [0.0; 3]);
        // walking at it climbs it
        run(&mut player, &world, &forward(0), 1.0);
        assert!(player.ladder.is_some() && player.origin[2] > 60.0, "{solid}: {player:?}");
        assert!((player.velocity[2] - CLIMB_SPEED).abs() < 1.0, "{player:?}");
        // letting go, the player hangs on
        let z = player.origin[2];
        run(&mut player, &world, &Command::default(), 1.0);
        assert!(player.ladder.is_some() && (player.origin[2] - z).abs() < 1.0, "{player:?}");
        // looking down and walking at it goes down
        let down = Command { angles: [70.0, 0.0, 0.0], ..forward(0) };
        run(&mut player, &world, &down, 0.2);
        assert!(player.origin[2] < z - 10.0, "{player:?}");
        // jump pushes off
        run(&mut player, &world, &Command { buttons: buttons::JUMP, ..Command::default() }, TICK);
        assert!(player.ladder.is_none() && player.velocity[0] < -200.0, "{player:?}");
        run(&mut player, &world, &Command::default(), 2.0);
        assert!(player.on_ground && player.origin[2] < 1.0 && player.origin[0] < 50.0, "{player:?}");
    }
}

#[test]
fn climbs_over_the_top_and_walks_off_at_the_bottom() {
    let world = ladder_wall(true);
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &forward(0), 3.0);
    assert!(player.on_ground && (player.origin[2] - 256.0).abs() < 1.0 && player.origin[0] > 100.0, "{player:?}");
    // at the bottom, walking back leaves the ladder
    let mut player = standing(&world, [0.0; 3]);
    run(&mut player, &world, &forward(0), 0.3);
    run(&mut player, &world, &Command { angles: [70.0, 0.0, 0.0], ..forward(0) }, 1.0);
    assert!(player.on_ground, "{player:?}");
    run(&mut player, &world, &Command { angles: [0.0, 180.0, 0.0], ..forward(0) }, 0.5);
    assert!(player.ladder.is_none() && player.origin[0] < 40.0, "{player:?}");
}
