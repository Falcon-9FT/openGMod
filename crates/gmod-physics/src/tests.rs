use super::*;

const TICK: f32 = 0.015;

fn near(a: Vec3, b: Vec3, within: f32) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() <= within)
}

/// A floor with its top at 0, as a map's brush gives it.
fn floor() -> Physics {
    let mut physics = Physics::new();
    let Shape::Hulls(hulls) = Shape::cuboid([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0]) else { unreachable!() };
    assert_eq!(physics.add_world(&hulls, &[], &[]), 1);
    physics
}

fn crate_body(origin: Vec3) -> Body {
    Body { mass: Some(30.0), ..Body::new(Shape::cuboid([-16.0; 3], [16.0; 3]), origin, [0.0; 3]) }
}

fn run(physics: &mut Physics, seconds: f32) {
    for _ in 0..(seconds / TICK).round() as usize {
        physics.step(TICK);
    }
}

#[test]
fn angles_turn_as_the_engine_turns_them() {
    for angles in [[0.0, 0.0, 0.0], [30.0, 45.0, 0.0], [-60.0, 170.0, 20.0], [10.0, -90.0, -45.0], [89.0, 0.0, 0.0]] {
        let back = super::angles(rotation(angles));
        assert!(near(back, angles, 0.01), "{angles:?} came back as {back:?}");
    }
    // forward along the yaw, pitch looking down
    let [forward, left, up] = axes([0.0, 90.0, 0.0]);
    assert!(near(forward, [0.0, 1.0, 0.0], 1e-6) && near(left, [-1.0, 0.0, 0.0], 1e-6) && near(up, [0.0, 0.0, 1.0], 1e-6));
    let turned = rotation([90.0, 0.0, 0.0]) * Vector::X;
    assert!(near(turned.to_array(), [0.0, 0.0, -1.0], 1e-5), "{turned:?}");
    // straight down there is no roll; the yaw goes into the left axis
    let down = super::angles(rotation([90.0, 30.0, 0.0]));
    assert!(near(down, [90.0, 30.0, 0.0], 0.05), "{down:?}");
}

#[test]
fn a_crate_falls_and_comes_to_rest_on_the_floor() {
    let mut physics = floor();
    let body = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    assert!((physics.mass(body) - 30.0).abs() < 0.01);
    // after a tenth of a second it falls as gravity says: v = g t
    run(&mut physics, 0.3);
    let v = physics.velocity(body);
    assert!((v[2] + DEFAULT_GRAVITY * 0.3).abs() < 15.0, "{v:?}");
    // then it lies on the floor, its bottom on the top, and falls asleep
    run(&mut physics, 3.0);
    let (origin, angles) = physics.pose(body).unwrap();
    assert!(near(origin, [0.0, 0.0, 16.0], 0.5) && near(angles, [0.0; 3], 0.5), "{origin:?} {angles:?}");
    assert!(physics.asleep(body) && physics.awake().is_empty());
    let (mins, maxs) = physics.bounds(body).unwrap();
    assert!(near(mins, [-16.0, -16.0, 0.0], 0.5) && near(maxs, [16.0, 16.0, 32.0], 0.5), "{mins:?} {maxs:?}");
}

#[test]
fn bodies_hit_each_other() {
    let mut physics = floor();
    let below = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
    let above = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    run(&mut physics, 3.0);
    let (top, _) = physics.pose(above).unwrap();
    let (bottom, _) = physics.pose(below).unwrap();
    assert!((top[2] - 48.0).abs() < 1.0 && (bottom[2] - 16.0).abs() < 1.0, "{top:?} {bottom:?}");
}

#[test]
fn frozen_bodies_stay() {
    let mut physics = floor();
    let body = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    physics.set_motion(body, false);
    assert!(!physics.motion(body));
    run(&mut physics, 1.0);
    assert_eq!(physics.pose(body).unwrap().0, [0.0, 0.0, 200.0]);
    // what falls on it stops there
    let other = physics.add(&crate_body([0.0, 0.0, 300.0])).unwrap();
    run(&mut physics, 2.0);
    assert!((physics.pose(other).unwrap().0[2] - 232.0).abs() < 1.0);
    physics.set_motion(body, true);
    run(&mut physics, 3.0);
    assert!((physics.pose(body).unwrap().0[2] - 16.0).abs() < 1.0);
}

#[test]
fn pushes_and_turns() {
    let mut physics = Physics::new();
    physics.set_gravity(0.0);
    let body = physics.add(&crate_body([0.0; 3])).unwrap();
    // a push of 300 kg·units/s gives 30 kg 10 units/s
    physics.apply_impulse(body, [300.0, 0.0, 0.0]);
    assert!(near(physics.velocity(body), [10.0, 0.0, 0.0], 1e-3), "{:?}", physics.velocity(body));
    physics.set_velocity(body, [0.0; 3]);
    // turning about its own axes, whichever way it is turned
    physics.set_pose(body, [0.0; 3], [0.0, 90.0, 0.0]);
    physics.set_angular_velocity(body, [0.0, 0.0, 45.0]);
    assert!(near(physics.angular_velocity(body), [0.0, 0.0, 45.0], 1e-3));
    run(&mut physics, 1.0);
    let (_, angles) = physics.pose(body).unwrap();
    assert!((angles[1] - 135.0).abs() < 1.0, "{angles:?}");
    // a push off the centre turns it
    physics.set_angular_velocity(body, [0.0; 3]);
    physics.apply_impulse_at(body, [0.0, 100.0, 0.0], [16.0, 0.0, 0.0]);
    assert!(physics.angular_velocity(body)[2].abs() > 1.0);
    // a point at its side moves with its turning
    let spin = physics.angular_velocity(body)[2].to_radians();
    let side = physics.velocity_at(body, [16.0, 0.0, 0.0]);
    assert!((side[1] - (physics.velocity(body)[1] + spin * 16.0)).abs() < 0.01, "{side:?}");
    assert!((physics.volume(body) - 32.0 * 32.0 * 32.0).abs() < 1.0);
    // heavier, slower
    physics.set_mass(body, 60.0);
    assert!((physics.mass(body) - 60.0).abs() < 0.01);
    assert!(near(physics.mass_center(body), [0.0; 3], 1e-3));
}

#[test]
fn without_gravity_or_collisions() {
    let mut physics = floor();
    let floating = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    physics.set_gravity_enabled(floating, false);
    let ghost = physics.add(&crate_body([100.0, 0.0, 50.0])).unwrap();
    physics.set_collisions(ghost, false);
    assert!(!physics.gravity_enabled(floating) && !physics.collisions(ghost));
    run(&mut physics, 1.0);
    assert_eq!(physics.pose(floating).unwrap().0[2], 200.0);
    assert!(physics.pose(ghost).unwrap().0[2] < -100.0, "falls through the floor");
}

#[test]
fn rays_find_bodies() {
    let mut physics = floor();
    let body = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    physics.set_motion(body, false);
    physics.step(TICK);
    let hit = physics.ray([0.0, 0.0, 300.0], [0.0, 0.0, -300.0], None).unwrap();
    assert_eq!(hit.body, body);
    assert!((hit.fraction - (300.0 - 116.0) / 600.0).abs() < 1e-3 && near(hit.normal, [0.0, 0.0, 1.0], 1e-3), "{hit:?}");
    // not the floor, and not the body skipped
    assert!(physics.ray([0.0, 0.0, 300.0], [0.0, 0.0, -300.0], Some(body)).is_none());
    assert!(physics.ray([500.0, 0.0, 300.0], [500.0, 0.0, -300.0], None).is_none());
}

#[test]
fn balls_and_what_has_no_volume() {
    let mut physics = floor();
    let ball = physics.add(&Body::new(Shape::Sphere(8.0), [0.0, 0.0, 100.0], [0.0; 3])).unwrap();
    run(&mut physics, 3.0);
    assert!((physics.pose(ball).unwrap().0[2] - 8.0).abs() < 0.5);
    // a ball of water... of the default density: 4/3 π r³
    let r = 8.0 * METRES_PER_UNIT;
    assert!((physics.mass(ball) - DEFAULT_DENSITY * 4.0 / 3.0 * std::f32::consts::PI * r * r * r).abs() < 0.01);
    let flat = Shape::Hulls(vec![vec![[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]]);
    assert!(physics.add(&Body::new(flat, [0.0; 3], [0.0; 3])).is_none());
    assert!(physics.add(&Body::new(Shape::Sphere(0.0), [0.0; 3], [0.0; 3])).is_none());
    physics.remove(ball);
    assert!(!physics.contains(ball));
}
