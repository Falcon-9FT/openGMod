use super::*;

const TICK: f32 = 0.015;

thread_local! {
    /// The step [`run`] takes: a tick of 66 a second, or of another tick rate.
    static STEP: std::cell::Cell<f32> = const { std::cell::Cell::new(TICK) };
}

/// Runs `test` with ticks of each of the rates the game may run at: 66 (GMod's), 33, 128, and
/// 100.
fn at_tick_rates(test: impl Fn()) {
    for rate in [66.6667f32, 33.0, 128.0, 100.0] {
        STEP.with(|s| s.set(1.0 / rate));
        test();
    }
    STEP.with(|s| s.set(TICK));
}

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
    let step = STEP.with(std::cell::Cell::get);
    for _ in 0..(seconds / step).round() as usize {
        physics.step(step);
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
    // then it lies on the floor, its bottom on the top, and falls asleep; it bounced, and went
    // a little aside (GMod's crates dropped so: 2 to 20 units, a few degrees)
    run(&mut physics, 3.0);
    let (origin, angles) = physics.pose(body).unwrap();
    assert!(near(origin, [0.0, 0.0, 16.0], 15.0) && (origin[2] - 16.0).abs() < 0.5, "{origin:?}");
    assert!(angles[0].abs() < 0.5 && angles[2].abs() < 0.5 && angles[1].abs() < 20.0, "{angles:?}");
    assert!(physics.asleep(body) && physics.awake().is_empty());
    let (mins, maxs) = physics.bounds(body).unwrap();
    assert!((mins[2] - 0.0).abs() < 0.5 && (maxs[2] - 32.0).abs() < 0.5, "{mins:?} {maxs:?}");
}

#[test]
fn a_body_of_no_friction_slides_on() {
    let mut physics = floor();
    let gripping = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
    let sliding = physics.add(&crate_body([0.0, 200.0, 16.0])).unwrap();
    physics.set_friction(sliding, 0.0);
    assert_eq!(physics.friction(sliding), 0.0);
    run(&mut physics, 0.5);
    for body in [gripping, sliding] {
        physics.set_velocity(body, [300.0, 0.0, 0.0]);
    }
    run(&mut physics, 1.0);
    // the floor holds the one back, and not the other: the less of the two goes
    let (held, _) = physics.pose(gripping).unwrap();
    let (slid, _) = physics.pose(sliding).unwrap();
    assert!(held[0] < 150.0 && slid[0] > 250.0, "{held:?} {slid:?}");
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

#[test]
fn steered_where_it_should_be() {
    let mut physics = floor();
    let body = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    // held in the air, a little off its centre, it goes there and turns so, gravity or not
    for _ in 0..100 {
        physics.steer(body, [50.0, 20.0, 150.0], [0.0, 90.0, 30.0], 0.05, 5000.0, 5000.0);
        physics.step(TICK);
    }
    let (origin, angles) = physics.pose(body).unwrap();
    assert!(near(origin, [50.0, 20.0, 150.0], 1.0) && near(angles, [0.0, 90.0, 30.0], 1.0), "{origin:?} {angles:?}");
    // no faster than it may
    physics.steer(body, [5050.0, 20.0, 150.0], [0.0, 90.0, 30.0], 0.05, 100.0, 5000.0);
    let v = physics.velocity(body);
    assert!((v[0] - 100.0).abs() < 1.0, "{v:?}");
    // the floor stops it
    for _ in 0..100 {
        physics.steer(body, [50.0, 20.0, -100.0], [0.0; 3], 0.05, 5000.0, 5000.0);
        physics.step(TICK);
    }
    assert!(physics.pose(body).unwrap().0[2] > 10.0, "{:?}", physics.pose(body));
}

#[test]
fn joined_to_the_world_and_to_each_other() {
    let mut physics = floor();
    let ground = physics.ground();
    // welded to the world it stays in the air, and what is welded to it stays with it
    let held = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    let hanging = physics.add(&crate_body([0.0, 0.0, 160.0])).unwrap();
    physics.join(held, ground, Joint::Weld, true, [0.0; 2]).unwrap();
    let weld = physics.join(hanging, held, Joint::Weld, true, [0.0; 2]).unwrap();
    run(&mut physics, 1.0);
    assert!(near(physics.pose(held).unwrap().0, [0.0, 0.0, 200.0], 0.5) && near(physics.pose(hanging).unwrap().0, [0.0, 0.0, 160.0], 0.5));
    // let go of the world, they fall together, as they were
    physics.unjoin(weld);
    assert!(!physics.joined(weld));
    physics.join(hanging, held, Joint::Weld, true, [0.0; 2]).unwrap();
    let both = physics.add(&crate_body([300.0, 0.0, 300.0])).unwrap();
    let other = physics.add(&crate_body([340.0, 0.0, 300.0])).unwrap();
    physics.join(both, other, Joint::Weld, false, [0.0; 2]).unwrap();
    run(&mut physics, 3.0);
    let (a, b) = (physics.pose(both).unwrap().0, physics.pose(other).unwrap().0);
    assert!((a[2] - 16.0).abs() < 1.0 && (b[2] - 16.0).abs() < 1.0 && (b[0] - a[0] - 40.0).abs() < 1.0, "{a:?} {b:?}");
    // the ground is not moved, let move, or taken away
    physics.set_motion(ground, true);
    physics.set_pose(ground, [0.0, 0.0, 50.0], [0.0; 3]);
    physics.remove(ground);
    assert!(physics.contains(ground) && !physics.motion(ground) && physics.pose(ground) == Some(([0.0; 3], [0.0; 3])));
}

#[test]
fn ropes_hinges_springs_and_sliders() {
    let mut physics = Physics::new();
    let ground = physics.ground();
    // on a rope of 100 from the top of the crate it hangs 100 below the hook
    let rope = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    physics
        .join(rope, ground, Joint::Rope { start: [0.0, 0.0, 116.0], end: [0.0, 0.0, 200.0], length: 100.0, rigid: false }, true, [0.0; 2])
        .unwrap();
    // on a hinge along y at its side it swings down under the hinge
    let door = physics.add(&crate_body([500.0, 0.0, 0.0])).unwrap();
    physics.join(door, ground, Joint::Hinge { point: [532.0, 0.0, 0.0], axis: [0.0, 1.0, 0.0] }, true, [0.0; 2]).unwrap();
    // on a slider along x it does not fall
    let slider = physics.add(&crate_body([-500.0, 0.0, 0.0])).unwrap();
    physics.join(slider, ground, Joint::Slider { point: [-500.0, 0.0, 0.0], axis: [1.0, 0.0, 0.0] }, true, [0.0; 2]).unwrap();
    physics.set_velocity(slider, [50.0, 0.0, 0.0]);
    // on a spring it hangs where its weight stretches it: m g / k
    let spring = physics.add(&crate_body([0.0, 500.0, 0.0])).unwrap();
    let (stiffness, damping) = (30.0 * 100.0, 30.0 * 20.0);
    physics
        .join(
            spring,
            ground,
            Joint::Spring {
                start: [0.0, 500.0, 16.0],
                end: [0.0, 500.0, 100.0],
                length: 50.0,
                stiffness,
                damping,
                relative_damping: 0.0,
                stretch_only: false,
            },
            true,
            [0.0; 2],
        )
        .unwrap();
    run(&mut physics, 4.0);
    let z = physics.pose(rope).unwrap().0[2];
    assert!((z - (200.0 - 100.0 - 16.0)).abs() < 2.0, "rope {z}");
    let (door_at, _) = physics.pose(door).unwrap();
    let swing = ((door_at[0] - 532.0).powi(2) + door_at[2].powi(2)).sqrt();
    assert!((swing - 32.0).abs() < 1.0 && door_at[2] < -20.0, "hinge {door_at:?}");
    let (slid, slid_angles) = physics.pose(slider).unwrap();
    assert!(slid[2].abs() < 0.5 && slid[0] > -400.0 && near(slid_angles, [0.0; 3], 0.5), "slider {slid:?}");
    let stretch = (DEFAULT_GRAVITY * METRES_PER_UNIT) * 30.0 / stiffness / METRES_PER_UNIT;
    let hangs = physics.pose(spring).unwrap().0[2];
    assert!((hangs - (100.0 - 50.0 - stretch - 16.0)).abs() < 3.0, "spring {hangs} {stretch}");
}

#[test]
fn what_breaks_and_what_goes_through() {
    let mut physics = floor();
    let ground = physics.ground();
    // 30 kg on a weld that takes 10 breaks; on one that takes 100 it stays
    let weak = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    let strong = physics.add(&crate_body([200.0, 0.0, 200.0])).unwrap();
    let snaps = physics.join(weak, ground, Joint::Weld, true, [10.0, 0.0]).unwrap();
    physics.join(strong, ground, Joint::Weld, true, [100.0, 0.0]).unwrap();
    run(&mut physics, 3.0);
    assert_eq!(physics.broken(), vec![snaps]);
    assert!(!physics.joined(snaps) && physics.broken().is_empty());
    assert!((physics.pose(weak).unwrap().0[2] - 16.0).abs() < 1.0 && (physics.pose(strong).unwrap().0[2] - 200.0).abs() < 1.0);
    // what does not collide with the frozen crate falls through it to the floor
    let frozen = physics.add(&crate_body([-200.0, 0.0, 100.0])).unwrap();
    physics.set_motion(frozen, false);
    let through = physics.add(&crate_body([-200.0, 0.0, 200.0])).unwrap();
    physics.join(through, frozen, Joint::NoCollide, false, [0.0; 2]).unwrap();
    run(&mut physics, 3.0);
    assert!((physics.pose(through).unwrap().0[2] - 16.0).abs() < 1.0, "{:?}", physics.pose(through));
}

#[test]
fn a_falling_crate_hits_the_floor_once() {
    let mut physics = floor();
    let body = physics.add(&Body { user: 7, ..crate_body([0.0, 0.0, 16.0 + 150.0]) }).unwrap();
    // it falls 150 units: it hits at sqrt(2 g h), about 424 units/s
    run(&mut physics, 0.8);
    let impacts = physics.impacts();
    let first = impacts.first().copied().unwrap_or_else(|| panic!("no impact: {:?}", physics.pose(body)));
    assert_eq!((first.user, first.other), (7, None));
    assert!(first.speed > 250.0 && first.speed < 650.0, "{first:?}");
    assert!(first.normal[2] < -0.9 && first.point[2].abs() < 2.0, "down into the floor: {first:?}");
    // lying there it makes no more
    run(&mut physics, 2.0);
    let resting = physics.impacts();
    assert!(resting.iter().all(|i| i.speed < first.speed / 4.0), "{resting:?}");
    run(&mut physics, 1.0);
    assert!(physics.impacts().is_empty());
}

/// A stick 40 long along x from `start`, as a ragdoll's forearm, that stops swinging.
fn stick(start: Vec3) -> Body {
    Body {
        mass: Some(5.0),
        damping: 2.0,
        rot_damping: 2.0,
        ..Body::new(Shape::cuboid([0.0, -2.0, -2.0], [40.0, 2.0, 2.0]), start, [0.0; 3])
    }
}

#[test]
fn ragdoll_joints_turn_as_far_as_their_limits() {
    // each stick hangs from a frozen post by its end and swings down about y as far as it may
    for (limits, pitch) in [
        ([[0.0, 0.0], [-45.0, 45.0], [0.0, 0.0]], 45.0),
        ([[0.0, 0.0], [-30.0, 80.0], [-10.0, 10.0]], 80.0),
        ([[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]], 0.0),
    ] {
        let mut physics = floor();
        let post = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
        physics.set_motion(post, false);
        let arm = physics.add(&stick([16.0, 0.0, 200.0])).unwrap();
        physics
            .join(post, arm, Joint::Ragdoll { point: [16.0, 0.0, 200.0], angles: [0.0; 3], limits, free: false }, false, [0.0; 2])
            .unwrap();
        run(&mut physics, 4.0);
        let (origin, angles) = physics.pose(arm).unwrap();
        assert!(near(origin, [16.0, 0.0, 200.0], 1.0), "{limits:?}: {origin:?}");
        // pointing down as far as it may, and turned no other way
        let (forward, left) = (axes(angles)[0], axes(angles)[1]);
        let down = f32::to_radians(pitch);
        assert!(near(forward, [down.cos(), 0.0, -down.sin()], 0.05) && near(left, [0.0, 1.0, 0.0], 0.05), "{limits:?}: {angles:?}");
    }
    // free, it swings down through straight down and on: in the air nothing damps it (as in
    // GMod), and nothing holds it back
    let mut physics = floor();
    let post = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    physics.set_motion(post, false);
    let arm = physics.add(&stick([16.0, 0.0, 200.0])).unwrap();
    let limits = [[-180.0, 180.0]; 3];
    physics.join(post, arm, Joint::Ragdoll { point: [16.0, 0.0, 200.0], angles: [0.0; 3], limits, free: false }, false, [0.0; 2]).unwrap();
    let mut lowest = 0.0f32;
    for _ in 0..100 {
        run(&mut physics, 0.04);
        let (origin, angles) = physics.pose(arm).unwrap();
        assert!(near(origin, [16.0, 0.0, 200.0], 1.0), "{origin:?}");
        lowest = lowest.min(axes(angles)[0][2]);
    }
    assert!(lowest < -0.99, "it never hung straight down: {lowest}");
    // the frame turns the axes: about its x, which is the world's y, the stick may not turn
    let mut physics = floor();
    let post = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    physics.set_motion(post, false);
    let arm = physics.add(&stick([16.0, 0.0, 200.0])).unwrap();
    let limits = [[0.0, 0.0], [-90.0, 90.0], [-90.0, 90.0]];
    physics
        .join(post, arm, Joint::Ragdoll { point: [16.0, 0.0, 200.0], angles: [0.0, 90.0, 0.0], limits, free: false }, false, [0.0; 2])
        .unwrap();
    run(&mut physics, 2.0);
    assert!(physics.pose(arm).unwrap().1[0].abs() < 3.0, "{:?}", physics.pose(arm));
}

#[test]
fn free_ragdoll_joints_hold_only_the_turn() {
    // a stick that may not turn falls away from its frozen post, still not turning
    for free in [false, true] {
        let mut physics = floor();
        let post = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
        physics.set_motion(post, false);
        let arm = physics.add(&stick([16.0, 0.0, 200.0])).unwrap();
        let joint = Joint::Ragdoll { point: [16.0, 0.0, 200.0], angles: [0.0; 3], limits: [[0.0; 2]; 3], free };
        physics.join(post, arm, joint, false, [0.0; 2]).unwrap();
        run(&mut physics, 2.0);
        let (origin, angles) = physics.pose(arm).unwrap();
        let fell = if free { origin[2] < 10.0 } else { near(origin, [16.0, 0.0, 200.0], 1.0) };
        assert!(fell && near(angles, [0.0; 3], 1.0), "{free}: {origin:?} {angles:?}");
    }
}

#[test]
fn ragdoll_joints_bend_far_as_knees_do() {
    // a stick standing on a post falls over about the frame's z, which is the world's -x, as
    // far as 145 degrees - past lying flat - and neither twists nor swings the other way
    let mut physics = floor();
    let post = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    physics.set_motion(post, false);
    let arm = physics.add(&Body { angles: [-90.0, 0.0, 0.0], ..stick([0.0, 0.0, 216.0]) }).unwrap();
    let limits = [[-30.0, 30.0], [-12.0, 12.0], [-12.0, 145.0]];
    physics
        .join(post, arm, Joint::Ragdoll { point: [0.0, 0.0, 216.0], angles: [-90.0, 0.0, 0.0], limits, free: false }, false, [0.0; 2])
        .unwrap();
    physics.set_angular_velocity(arm, [0.0, 0.0, 30.0]);
    run(&mut physics, 5.0);
    let (origin, angles) = physics.pose(arm).unwrap();
    let bent = f32::to_radians(145.0);
    assert!(near(origin, [0.0, 0.0, 216.0], 1.0), "{origin:?}");
    assert!(near(axes(angles)[0], [0.0, bent.sin(), bent.cos()], 0.05), "{angles:?}: {:?}", axes(angles));
    // it turned about its up, the world's -x, only: no twist, no swing about y
    assert!(axes(angles)[2][0] < -0.99, "{:?}", axes(angles));
}

#[test]
fn what_is_harder_to_turn_turns_slower() {
    let mut physics = Physics::new();
    physics.set_gravity(0.0);
    let plain = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    let heavy = physics.add(&Body { inertia: 10.0, ..crate_body([200.0, 0.0, 100.0]) }).unwrap();
    for id in [plain, heavy] {
        physics.apply_torque_impulse(id, [0.0, 0.0, 100.0]);
    }
    physics.step(TICK);
    let (a, b) = (physics.angular_velocity(plain)[2], physics.angular_velocity(heavy)[2]);
    assert!(a > 0.0 && (a / b - 10.0).abs() < 0.1 && (physics.mass(heavy) - 30.0).abs() < 0.01, "{a} {b}");
    // heavier, it is as much harder to turn again
    physics.set_mass(heavy, 60.0);
    physics.set_mass(plain, 60.0);
    for id in [plain, heavy] {
        physics.set_angular_velocity(id, [0.0; 3]);
        physics.apply_torque_impulse(id, [0.0, 0.0, 100.0]);
    }
    physics.step(TICK);
    let (a, b) = (physics.angular_velocity(plain)[2], physics.angular_velocity(heavy)[2]);
    assert!((a / b - 10.0).abs() < 0.1, "{a} {b}");
}

#[test]
fn bodies_set_apart_go_through_each_other() {
    let mut physics = floor();
    // two crates in one place lie on the floor in one place, and others push apart
    let a = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    let b = physics.add(&crate_body([8.0, 0.0, 100.0])).unwrap();
    physics.set_apart(a, b, true);
    assert!(physics.apart(a, b) && physics.apart(b, a));
    run(&mut physics, 3.0);
    let (pa, pb) = (physics.pose(a).unwrap().0, physics.pose(b).unwrap().0);
    assert!(near(pa, [0.0, 0.0, 16.0], 6.0) && near(pb, [8.0, 0.0, 16.0], 6.0), "{pa:?} {pb:?}");
    // together again, they push each other apart
    physics.set_apart(a, b, false);
    assert!(!physics.apart(a, b));
    run(&mut physics, 2.0);
    let (pa, pb) = (physics.pose(a).unwrap().0, physics.pose(b).unwrap().0);
    let apart = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
    assert!(apart > 30.0, "{pa:?} {pb:?}");
    // a body gone is apart from nothing
    physics.set_apart(a, b, true);
    physics.remove(b);
    assert!(!physics.apart(a, b));
}

#[test]
fn aloof_bodies_go_through_what_moves() {
    let mut physics = floor();
    // through a crate that moves, which feels nothing, onto the floor
    let below = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
    let aloof = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    physics.set_aloof(aloof, true);
    run(&mut physics, 2.0);
    let (pa, pb) = (physics.pose(aloof).unwrap().0, physics.pose(below).unwrap().0);
    assert!(near(pa, [0.0, 0.0, 16.0], 6.0) && near(pb, [0.0, 0.0, 16.0], 0.5), "{pa:?} {pb:?}");
    // one that does not move holds it, and so does another aloof one
    physics.set_motion(below, false);
    physics.set_pose(aloof, [0.0, 0.0, 100.0], [0.0; 3]);
    let other = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    physics.set_aloof(other, true);
    // bouncing as little as anything does, which may still tip them over the edge (GMod's
    // crates dropped on crates mostly fall off)
    physics.set_elasticity(aloof, 0.0);
    physics.set_elasticity(other, 0.0);
    run(&mut physics, 3.0);
    let (pa, po) = (physics.pose(aloof).unwrap().0, physics.pose(other).unwrap().0);
    assert!(near(pa, [0.0, 0.0, 48.0], 1.0) && near(po, [0.0, 0.0, 80.0], 2.0), "{pa:?} {po:?}");
}

#[test]
fn rays_into_one_body() {
    let mut physics = floor();
    let a = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    let b = physics.add(&crate_body([100.0, 0.0, 100.0])).unwrap();
    let hit = physics.ray_into([-100.0, 0.0, 100.0], [200.0, 0.0, 100.0], b).unwrap();
    assert!(hit.body == b && (hit.fraction - 184.0 / 300.0).abs() < 0.01 && near(hit.normal, [-1.0, 0.0, 0.0], 0.01), "{hit:?}");
    assert!(physics.ray_into([-100.0, 0.0, 200.0], [200.0, 0.0, 200.0], a).is_none());
    // a body just moved is found where it went, before any step moves its shape along
    physics.set_pose(b, [100.0, 0.0, 300.0], [0.0; 3]);
    assert!(physics.ray_into([-100.0, 0.0, 300.0], [200.0, 0.0, 300.0], b).is_some());
    assert!(physics.ray_into([-100.0, 0.0, 100.0], [200.0, 0.0, 100.0], b).is_none());
    let (lo, hi) = physics.bounds(b).unwrap();
    assert!(lo[2] > 250.0 && hi[2] < 350.0 && lo[2] < 300.0 && hi[2] > 300.0, "{lo:?} {hi:?}");
    // in each other or not, as they are now
    assert!(!physics.overlapping(a, b));
    physics.set_pose(b, [20.0, 0.0, 110.0], [0.0; 3]);
    assert!(physics.overlapping(a, b) && physics.overlapping(b, a));
}

#[test]
fn joined_bodies_put_to_sleep_stay_asleep() {
    // a ragdoll that starts asleep: joined, then put to sleep before any step
    let mut physics = floor();
    let post = physics.add(&crate_body([0.0, 0.0, 200.0])).unwrap();
    let arm = physics.add(&stick([16.0, 0.0, 200.0])).unwrap();
    let joint =
        Joint::Ragdoll { point: [16.0, 0.0, 200.0], angles: [0.0; 3], limits: [[0.0, 0.0], [-30.0, 30.0], [-45.0, 45.0]], free: false };
    physics.join(post, arm, joint, false, [0.0; 2]).unwrap();
    physics.sleep(post);
    physics.sleep(arm);
    run(&mut physics, 1.0);
    assert!(physics.asleep(post) && physics.asleep(arm) && near(physics.pose(post).unwrap().0, [0.0, 0.0, 200.0], 0.01));
    // woken, they fall together
    physics.wake(arm);
    run(&mut physics, 1.0);
    assert!(physics.pose(post).unwrap().0[2] < 150.0 && physics.pose(arm).unwrap().0[2] < 150.0);
}

#[test]
fn movers_carry_and_push_what_they_meet() {
    let mut physics = floor();
    // a platform with a crate asleep on it, and a crate beside where it will go
    let Shape::Hulls(slab) = Shape::cuboid([-64.0, -64.0, -8.0], [64.0, 64.0, 0.0]) else { unreachable!() };
    let lift = physics.add_mover(&slab, [0.0, 0.0, 8.0], [0.0; 3], 7).unwrap();
    assert_eq!(physics.user(lift), Some(7));
    assert!(!physics.motion(lift), "nothing pushes it");
    let on_top = physics.add(&crate_body([0.0, 0.0, 30.0])).unwrap();
    run(&mut physics, 2.0);
    physics.sleep(on_top);
    // up 100 units in a second: what is on it goes up with it
    for tick in 1..=66 {
        let z = 8.0 + 100.0 * tick as f32 / 66.0;
        physics.move_mover(lift, [0.0, 0.0, z], [0.0; 3]);
        physics.step(TICK);
    }
    let (at, _) = physics.pose(lift).unwrap();
    assert!(near(at, [0.0, 0.0, 108.0], 0.1), "{at:?}");
    let (crate_at, _) = physics.pose(on_top).unwrap();
    assert!(crate_at[2] > 115.0, "carried up: {crate_at:?}");
    // sideways into a crate standing on the floor: it is pushed out of the way
    let beside = physics.add(&crate_body([150.0, 0.0, 16.0])).unwrap();
    run(&mut physics, 1.0);
    physics.sleep(beside);
    for tick in 1..=66 {
        let x = 300.0 * tick as f32 / 66.0;
        physics.move_mover(lift, [x, 0.0, 20.0], [0.0; 3]);
        physics.step(TICK);
    }
    let (pushed, _) = physics.pose(beside).unwrap();
    assert!(pushed[0] > 320.0, "pushed along: {pushed:?}");
}

/// A trolleybus's wheel: a heavy ball held to a heavier body by two rigid ropes across it and
/// springs to where it hangs. The body spun hard, the wheel goes round with it.
#[test]
fn a_wheel_on_rigid_ropes_goes_round_with_its_body() {
    let mut physics = Physics::new();
    physics.set_gravity(0.0);
    let body = physics
        .add(&Body {
            mass: Some(7466.0),
            ..Body::new(Shape::cuboid([-230.0, -48.0, -49.0], [216.0, 48.0, 74.0]), [0.0, 0.0, 100.0], [0.0; 3])
        })
        .unwrap();
    let wheel_at = [112.0, 40.0, 100.0 - 45.0];
    let wheel = physics.add(&Body { mass: Some(933.0), ..Body::new(Shape::Sphere(20.0), wheel_at, [0.0; 3]) }).unwrap();
    let distance = |a: Vec3, b: Vec3| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    for (dx, rigid) in [(-40.0, true), (40.0, true), (0.0, false)] {
        // across the body, level with the wheel, as Trolleybus System ties them
        let end = [112.0 + dx, -40.0, 100.0 - 45.0];
        let joint = Joint::Rope { start: wheel_at, end, length: distance(wheel_at, end), rigid };
        physics.join(wheel, body, joint, false, [0.0; 2]).unwrap();
    }
    for _ in 0..6 {
        let spring = Joint::Spring {
            start: wheel_at,
            end: wheel_at,
            length: 0.0,
            stiffness: 50000.0,
            damping: 1300.0,
            relative_damping: 1300.0,
            stretch_only: false,
        };
        physics.join(wheel, body, spring, false, [0.0; 2]).unwrap();
    }
    let local = |physics: &Physics| {
        let (origin, angles) = physics.pose(body).unwrap();
        let (point, _) = physics.pose(wheel).unwrap();
        let d = Vector::from(point) - Vector::from(origin);
        (rotation(angles).inverse() * d).to_array()
    };
    run(&mut physics, 0.3);
    let rest = local(&physics);
    let mut worst: f32 = 0.0;
    for tick in 0..100 {
        physics.set_angular_velocity(body, if tick < 4 { [0.0, 0.0, 600.0] } else { [0.0; 3] });
        physics.step(TICK);
        worst = worst.max(distance(local(&physics), rest));
    }
    // GMod, measured: 4 units at most (rigid ropes are rods there)
    assert!(worst < 5.0, "the wheel went {worst} units from its place");
}

/// Debris (1) collides with nothing but what has no group, as Half-Life 2's rules have it.
fn rule(a: i64, b: i64) -> bool {
    !(a == 1 && b == 1)
}

#[test]
fn collision_groups_say_what_collides() {
    let mut physics = floor();
    physics.set_group_rule(rule);
    let below = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
    physics.set_motion(below, false);
    let below2 = physics.add(&crate_body([200.0, 0.0, 16.0])).unwrap();
    physics.set_motion(below2, false);
    // the world only: through the crate, onto the floor
    let world_only = physics.add(&crate_body([0.0, 0.0, 100.0])).unwrap();
    physics.set_collision_group(world_only, WORLD_ONLY);
    // debris on debris: through it as well
    physics.set_collision_group(below2, 1);
    let debris = physics.add(&crate_body([200.0, 0.0, 100.0])).unwrap();
    physics.set_collision_group(debris, 1);
    // nothing at all, not solid: through the floor too
    let ghost = physics.add(&crate_body([400.0, 0.0, 100.0])).unwrap();
    physics.set_collisions(ghost, false);
    run(&mut physics, 2.0);
    let z = |id| physics.pose(id).unwrap().0[2];
    assert!((z(world_only) - 16.0).abs() < 2.0, "{}", z(world_only));
    assert!((z(debris) - 16.0).abs() < 2.0, "{}", z(debris));
    assert!(z(ghost) < -200.0, "{}", z(ghost));
}

#[test]
fn a_concrete_barrier_lying_across_a_crest_of_the_ground_falls_asleep() {
    at_tick_rates(barrier_on_crests);
}

fn barrier_on_crests() {
    // where gm_construct has it, and nearer the middle of the world
    let step = STEP.with(std::cell::Cell::get);
    for at in [[-3952.0, 4100.0, -100.0], [-250.0, 250.0, -100.0], [-8000.0, 8000.0, 0.0]] {
        let fastest = barrier_on_crest(at);
        assert_eq!(fastest, 0.0, "a tick of {step} s: at {at:?} still moving, as fast as {fastest} units/s after 5 s");
    }
}

/// A concrete barrier lying across a crest of ground at `at`: 0 when it falls asleep, else the
/// fastest it moved after the first 5 seconds of 20.
fn barrier_on_crest(at: Vec3) -> f32 {
    // gm_construct's ground at (-3952, 4100, -100): two slopes of a displacement meeting
    let ground: [[Vec3; 3]; 12] = [
        [[100.0, -101.29, -0.38], [0.0, -100.07, 0.0], [100.0, -27.96, -0.62]],
        [[100.0, -27.96, -0.62], [0.0, -100.07, 0.0], [0.0, -27.05, 0.0]],
        [[100.0, -27.96, -0.62], [0.0, -27.05, 0.0], [0.0, 45.96, 0.0]],
        [[100.0, -27.96, -0.62], [0.0, 45.96, 0.0], [100.0, 45.36, -0.67]],
        [[100.0, 45.36, -0.67], [0.0, 45.96, 0.0], [100.0, 118.68, -0.67]],
        [[100.0, 118.68, -0.67], [0.0, 45.96, 0.0], [0.0, 118.98, 0.0]],
        [[0.0, -100.07, 0.0], [-100.0, -98.86, -0.25], [-100.0, -26.14, -0.41]],
        [[0.0, -100.07, 0.0], [-100.0, -26.14, -0.41], [0.0, -27.05, 0.0]],
        [[0.0, -27.05, 0.0], [-100.0, -26.14, -0.41], [0.0, 45.96, 0.0]],
        [[0.0, 45.96, 0.0], [-100.0, -26.14, -0.41], [-100.0, 46.57, -0.7]],
        [[0.0, 45.96, 0.0], [-100.0, 46.57, -0.7], [-100.0, 119.29, -0.85]],
        [[0.0, 45.96, 0.0], [-100.0, 119.29, -0.85], [0.0, 118.98, 0.0]],
    ];
    let mut physics = Physics::new();
    let corners: Vec<Vec3> = ground.iter().flatten().map(|p| [p[0] + at[0], p[1] + at[1], p[2] + at[2]]).collect();
    let triangles: Vec<[u32; 3]> = (0..12).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect();
    physics.add_world(&[], &corners, &triangles);
    // models/props_c17/concrete_barrier001a.mdl, as its .phy has it
    let hulls = vec![
        vec![
            [-4.959, 55.995, 12.098],
            [-2.732, -56.069, 48.072],
            [-4.959, -56.069, 12.098],
            [-2.732, 55.995, 48.072],
            [2.660, 55.995, 48.072],
            [2.660, -56.069, 48.072],
            [4.831, -56.069, 12.098],
            [4.831, 55.995, 12.098],
        ],
        vec![
            [5.591, -56.069, 13.624],
            [-5.581, -56.069, 13.624],
            [6.048, 55.995, 13.624],
            [-5.581, 55.995, 13.624],
            [10.188, -56.069, -0.183],
            [-10.252, -56.069, -0.183],
            [10.188, 55.995, -0.183],
            [-10.252, 55.995, -0.183],
        ],
    ];
    let body = physics
        .add(&Body { mass: Some(75.0), ..Body::new(Shape::Hulls(hulls), [at[0] + 2.0, at[1], at[2] + 2.0], [0.0, 75.0, 0.0]) })
        .unwrap();
    let mut fastest: f32 = 0.0;
    for second in 0..20 {
        run(&mut physics, 1.0);
        if second >= 5 {
            let v = physics.velocity(body);
            fastest = fastest.max((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
        }
    }
    if physics.asleep(body) { 0.0 } else { fastest }
}

#[test]
fn what_falls_fast_does_not_go_through_the_ground() {
    at_tick_rates(falls_fast);
}

fn falls_fast() {
    for speed in [1000.0f32, 3000.0, 6000.0] {
        for small in [4.0f32, 16.0] {
            // ground as a displacement makes it: triangles, no thickness
            let mut physics = Physics::new();
            let corners = [
                [-512.0, -512.0, 0.0],
                [512.0, -512.0, 0.0],
                [512.0, 512.0, 0.0],
                [-512.0, -512.0, 0.0],
                [512.0, 512.0, 0.0],
                [-512.0, 512.0, 0.0],
            ];
            physics.add_world(&[], &corners, &[[0, 1, 2], [3, 4, 5]]);
            // and a floor of a brush 8 units thick beside it
            let Shape::Hulls(hulls) = Shape::cuboid([600.0, -512.0, -8.0], [1600.0, 512.0, 0.0]) else { unreachable!() };
            physics.add_world(&hulls, &[], &[]);
            let mut bodies = Vec::new();
            for x in [0.0, 1100.0] {
                let body = physics
                    .add(&Body { mass: Some(5.0), ..Body::new(Shape::cuboid([-small; 3], [small; 3]), [x, 0.0, 300.0], [0.0; 3]) })
                    .unwrap();
                physics.set_velocity(body, [0.0, 0.0, -speed]);
                bodies.push(body);
            }
            run(&mut physics, 2.0);
            for body in bodies {
                let (origin, _) = physics.pose(body).unwrap();
                assert!(origin[2] > 0.0, "a box of {} units at {speed} units/s went through to {origin:?}", small * 2.0);
            }
        }
    }
}

#[test]
fn a_car_held_on_its_wheels_by_rods_and_springs_stands_still() {
    at_tick_rates(car_stands_still);
}

fn car_stands_still() {
    // simfphys' Volga as GMod joins it: a body, and four wheels each held by two rods to the
    // far side, a rope that limits how far it hangs, a spring, and a ball joint that keeps it
    // from turning
    let mut physics = floor();
    let ground = 3.39;
    let car = physics
        .add(&Body {
            mass: Some(1265.0),
            ..Body::new(Shape::cuboid([-105.7, -42.3, 0.0], [96.0, 42.6, 66.1]), [0.0, 0.0, ground], [0.0; 3])
        })
        .unwrap();
    let on_car = |p: Vec3| [p[0], p[1], p[2] + ground];
    for (x, y) in [(64.0, 34.0), (64.0, -34.0), (-55.0, 34.0), (-55.0, -34.0)] {
        let centre = on_car([x, y, 12.0]);
        let wheel = physics.add(&Body { mass: Some(42.19), ..Body::new(Shape::Sphere(15.4), centre, [0.0; 3]) }).unwrap();
        physics.set_friction(wheel, 1.0);
        let across = if y > 0.0 { -150.0 } else { 150.0 };
        for along in [75.0, -75.0] {
            let rod = Joint::Rope { start: on_car([x + along, y + across, 6.5]), end: centre, length: 167.7, rigid: true };
            physics.join(car, wheel, rod, false, [0.0; 2]).unwrap();
        }
        let limiter = Joint::Rope { start: on_car([x, y + across * 0.4, 13.0]), end: centre, length: 60.2, rigid: false };
        physics.join(car, wheel, limiter, false, [0.0; 2]).unwrap();
        let spring = Joint::Spring {
            start: [centre[0], centre[1], centre[2] + 6.5],
            end: on_car([x, y, 6.5]),
            length: 6.5,
            stiffness: 25000.0,
            damping: 1300.0,
            relative_damping: 1300.0,
            stretch_only: false,
        };
        physics.join(wheel, car, spring, false, [0.0; 2]).unwrap();
        let held = Joint::Ragdoll { point: centre, angles: [0.0; 3], limits: [[-0.01, 0.01]; 3], free: true };
        physics.join(car, wheel, held, false, [0.0; 2]).unwrap();
    }
    run(&mut physics, 4.0);
    let (_, angles) = physics.pose(car).unwrap();
    run(&mut physics, 6.0);
    let (_, later) = physics.pose(car).unwrap();
    let v = physics.velocity(car);
    let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    assert!(
        speed < 0.5 && (later[1] - angles[1]).abs() < 0.1,
        "it creeps: {speed} units/s, turned {} degrees in 6 s",
        later[1] - angles[1]
    );
}

#[test]
fn a_shadow_holds_back_some_of_its_own_speed_and_is_pushed_to_where_it_is_to_be() {
    // as IVP's ComputeController: 0.8 of the speed held back, no more than the damping allows
    let v = Vector::new(10.0, 0.0, 0.0);
    assert!((shadow_control(v, Vector::ZERO, 0.0, 100.0, 0.0, 0.8).x - 2.0).abs() < 1e-5);
    assert!((shadow_control(v, Vector::ZERO, 0.0, 1.0, 0.0, 0.8).x - 9.0).abs() < 1e-5);
    // and pushed by the way to go times the scale, no faster than its most
    let pushed = shadow_control(Vector::ZERO, Vector::new(0.0, 1.0, 0.0), 5.0, 0.0, 20.0, 0.8);
    assert!((pushed.y - 5.0).abs() < 1e-5);

    // the physics gun's: up in the air, steered 100 units away and held there
    let mut physics = Physics::new();
    physics.set_gravity(0.0);
    let body = physics.add(&crate_body([0.0, 0.0, 500.0])).unwrap();
    let held = Shadow {
        origin: [100.0, 0.0, 500.0],
        angles: [0.0, 90.0, 0.0],
        seconds: 0.05,
        max_speed: 5000.0,
        max_damp_speed: 10000.0,
        max_angular: 5000.0,
        max_damp_angular: 10000.0,
        damp_factor: 0.8,
        teleport_distance: 0.0,
    };
    for _ in 0..60 {
        physics.shadow(body, &held, TICK);
        physics.step(TICK);
    }
    let (origin, angles) = physics.pose(body).unwrap();
    assert!(near(origin, [100.0, 0.0, 500.0], 0.5) && near(angles, [0.0, 90.0, 0.0], 0.5), "{origin:?} {angles:?}");
    // far beyond its teleport distance it is put there at once
    let far = Shadow { origin: [5000.0, 0.0, 500.0], teleport_distance: 1000.0, ..held };
    physics.shadow(body, &far, TICK);
    assert!(near(physics.pose(body).unwrap().0, [5000.0, 0.0, 500.0], 0.01));
}

#[test]
fn nothing_goes_faster_than_the_limits() {
    let mut physics = Physics::new();
    physics.set_gravity(0.0);
    let body = physics.add(&crate_body([0.0, 0.0, 500.0])).unwrap();
    physics.set_velocity(body, [20000.0, 0.0, 0.0]);
    physics.step(TICK);
    assert!((physics.velocity(body)[0] - MAX_VELOCITY).abs() < 1.0, "{:?}", physics.velocity(body));
    physics.set_speed_limits(1000.0, 360.0);
    physics.step(TICK);
    assert!((physics.velocity(body)[0] - 1000.0).abs() < 1.0);
}

#[test]
fn friction_is_the_two_surfaces_multiplied() {
    // a crate of glass (0.5) pushed along the floor (0.8) at 400 units/s slides as 0.4 does:
    // v² / (2 μ g), 333 units (GMod: 349, the crate turning as it went)
    let mut physics = floor();
    let body = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
    physics.set_friction(body, 0.5);
    run(&mut physics, 0.5);
    physics.set_velocity(body, [400.0, 0.0, 0.0]);
    run(&mut physics, 3.0);
    let slid = physics.pose(body).unwrap().0[0];
    assert!((slid - 333.0).abs() < 25.0, "slid {slid}");
}

#[test]
fn what_slides_over_the_seams_of_brushes_does_not_trip_on_them() {
    // a floor of brushes side by side, as maps are made, and a crate sliding over their
    // seams fast; it went head over heels on the first (the face between two brushes caught
    // its edge)
    at_tick_rates(|| {
        for speed in [300.0f32, 600.0, 1000.0] {
            let mut physics = Physics::new();
            let hulls: Vec<Vec<Vec3>> = (0..24)
                .flat_map(|i| {
                    let x = -256.0 + i as f32 * 128.0;
                    let Shape::Hulls(hulls) = Shape::cuboid([x, -512.0, -64.0], [x + 128.0, 512.0, 0.0]) else { unreachable!() };
                    hulls
                })
                .collect();
            physics.add_world(&hulls, &[], &[]);
            let body = physics.add(&crate_body([-200.0, 0.0, 16.0])).unwrap();
            physics.set_friction(body, 0.1);
            run(&mut physics, 0.5);
            physics.set_velocity(body, [speed, 0.0, 0.0]);
            // until it is near the floor's end
            for step in 0..90 {
                run(&mut physics, 0.025);
                let (origin, angles) = physics.pose(body).unwrap();
                assert!(
                    angles[0].abs() < 1.0 && angles[2].abs() < 1.0 && (origin[2] - 16.0).abs() < 1.0,
                    "at {speed} units/s it tripped by {origin:?} {angles:?} ({step})"
                );
            }
        }
    });
}

#[test]
fn what_falls_bounces_by_both_surfaces() {
    // a ball dropped on the floor (0.25) goes back up by √(e₁ e₂) of the speed it hit at, no
    // less than 0.2 and no more than all of it (GMod: a ball on concrete, 0.2, at 0.25 0.228,
    // at 0.5 0.321, at 1000 1.0; of dirt, 0.01, 0.204, on grass too)
    at_tick_rates(|| {
        for (elasticity, ratio) in [(0.0f32, 0.2f32), (0.01, 0.2), (0.25, 0.25), (0.5, 0.354), (2.0, 0.707), (1000.0, 1.0)] {
            let mut physics = floor();
            let ball = Body { mass: Some(20.0), ..Body::new(Shape::Sphere(12.0), [0.0, 0.0, 212.0], [0.0; 3]) };
            let ball = physics.add(&ball).unwrap();
            physics.set_elasticity(ball, elasticity);
            let (mut hit, mut back) = (0.0f32, 0.0f32);
            let step = STEP.with(std::cell::Cell::get);
            let mut last = 0.0;
            for _ in 0..(1.5 / step) as usize {
                physics.step(step);
                let v = physics.velocity(ball)[2];
                if last < -50.0 && v >= 0.0 && hit == 0.0 {
                    hit = -last;
                }
                if hit > 0.0 {
                    back = back.max(v);
                }
                last = v;
            }
            let got = if hit > 0.0 { back / hit } else { 0.0 };
            assert!((got - ratio).abs() < 0.06, "at {elasticity} it bounced by {got} (hit at {hit})");
        }
    });
}

#[test]
fn what_touches_the_world_touches_the_face_it_touches() {
    // a floor whose top is slippery (0.2) and sides are not, as a brush of ice: a crate of
    // glass (0.5) pushed along the top at 200 units/s slides as 0.1 does, v² / (2 μ g), 333
    // units; and the top is bouncy as its surface says
    let Shape::Hulls(hulls) = Shape::cuboid([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0]) else { unreachable!() };
    let rough = Surface { friction: 0.8, elasticity: 0.0 };
    let top = Surface { friction: 0.2, elasticity: 1.0 };
    let mut faces = vec![([0.0, 0.0, 1.0], top), ([0.0, 0.0, -1.0], rough)];
    faces.extend([[1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, -1.0, 0.0]].map(|n| (n, rough)));
    let mut physics = Physics::new();
    physics.add_world_made_of(&hulls, &[faces], &[], &[], &[]);
    let body = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
    physics.set_friction(body, 0.5);
    physics.set_elasticity(body, 0.0);
    run(&mut physics, 0.5);
    physics.set_velocity(body, [200.0, 0.0, 0.0]);
    run(&mut physics, 3.0);
    let slid = physics.pose(body).unwrap().0[0];
    assert!((slid - 333.0).abs() < 25.0, "slid {slid}");
    // a ball of 0.25 off a top of 1 bounces by 0.5
    let ball = Body { mass: Some(20.0), ..Body::new(Shape::Sphere(12.0), [-500.0, 0.0, 212.0], [0.0; 3]) };
    let ball = physics.add(&ball).unwrap();
    let (mut hit, mut back, mut last) = (0.0f32, 0.0f32, 0.0f32);
    for _ in 0..100 {
        run(&mut physics, 0.015);
        let v = physics.velocity(ball)[2];
        if last < -50.0 && v >= 0.0 && hit == 0.0 {
            hit = -last;
        }
        if hit > 0.0 {
            back = back.max(v);
        }
        last = v;
    }
    assert!((back / hit - 0.5).abs() < 0.06, "bounced by {} ({hit})", back / hit);
}

#[test]
fn a_surface_of_triangles_is_touched_by_the_triangle_touched() {
    // two triangles of a ground of ice (0.2), two of rough ground (0.8)
    let vertices = vec![
        [-1024.0, -1024.0, 0.0],
        [0.0, -1024.0, 0.0],
        [0.0, 1024.0, 0.0],
        [-1024.0, 1024.0, 0.0],
        [1024.0, -1024.0, 0.0],
        [1024.0, 1024.0, 0.0],
    ];
    let triangles = vec![[0, 2, 1], [0, 3, 2], [1, 5, 4], [1, 2, 5]];
    let (ice, rough) = (Surface { friction: 0.2, elasticity: 0.0 }, Surface { friction: 0.8, elasticity: 0.0 });
    let mut slid = Vec::new();
    for x in [-500.0f32, 300.0] {
        let mut physics = Physics::new();
        physics.add_world_made_of(&[], &[], &vertices, &triangles, &[ice, ice, rough, rough]);
        let body = physics.add(&crate_body([x, 0.0, 16.0])).unwrap();
        physics.set_friction(body, 0.5);
        physics.set_elasticity(body, 0.0);
        run(&mut physics, 0.5);
        physics.set_velocity(body, [200.0, 0.0, 0.0]);
        run(&mut physics, 3.0);
        slid.push(physics.pose(body).unwrap().0[0] - x);
    }
    // on ice 333 units, on rough ground 83
    assert!((slid[0] - 333.0).abs() < 30.0 && (slid[1] - 83.0).abs() < 15.0, "slid {slid:?}");
}

#[test]
fn a_spring_that_only_stretches_pulls_and_never_pushes() {
    // two cans without gravity on a spring 100 long (GMod's constraint.Elastic, measured: put
    // 50 apart, one that only stretches leaves them there; put 150 apart, it pulls them in)
    for (stretch_only, from, nearer) in [(false, 50.0f32, false), (true, 50.0, false), (false, 150.0, true), (true, 150.0, true)] {
        let mut physics = Physics::new();
        physics.set_gravity(0.0);
        let anchor = physics.add(&Body { mass: Some(1.0), ..Body::new(Shape::Sphere(2.0), [0.0, 0.0, 0.0], [0.0; 3]) }).unwrap();
        physics.set_motion(anchor, false);
        let can = physics.add(&Body { mass: Some(1.0), ..Body::new(Shape::Sphere(2.0), [0.0, 0.0, -100.0], [0.0; 3]) }).unwrap();
        let spring = Joint::Spring {
            start: [0.0; 3],
            end: [0.0, 0.0, -100.0],
            length: 100.0,
            stiffness: 100.0,
            damping: 0.0,
            relative_damping: 0.0,
            stretch_only,
        };
        physics.join(anchor, can, spring, true, [0.0; 2]).unwrap();
        physics.set_pose(can, [0.0, 0.0, -from], [0.0; 3]);
        run(&mut physics, 0.1);
        let apart = -physics.pose(can).unwrap().0[2];
        let moved = (apart - from).abs() > 5.0;
        match (stretch_only, nearer) {
            (true, false) => assert!(!moved, "only stretching, it pushed: {apart}"),
            (false, true) => assert!(apart < from - 5.0, "it did not pull: {apart}"),
            (true, true) => {
                assert!(apart < from - 5.0, "it did not pull: {apart}");
                // and nearer than its length, it lets the can go on (GMod: 71.6 at 0.2 s)
                run(&mut physics, 0.1);
                let later = -physics.pose(can).unwrap().0[2];
                assert!(later < 85.0, "it held the can back: {later}");
            }
            (false, false) => assert!(apart > from + 5.0, "it did not push: {apart}"),
        }
    }
}

#[test]
fn damping_holds_back_only_what_touches_something() {
    // GMod, measured: a crate damped by 5 going 100 units/s keeps going so in the air, and on
    // a floor without friction keeps e⁻⁵ of it a second (33 after a fifth of a second)
    let mut physics = floor();
    let flying = physics.add(&crate_body([0.0, 0.0, 300.0])).unwrap();
    let sliding = physics.add(&crate_body([0.0, 200.0, 16.0])).unwrap();
    physics.set_friction(sliding, 0.0);
    run(&mut physics, 0.3);
    for body in [flying, sliding] {
        physics.set_damping(body, 5.0, 0.0);
        physics.set_velocity(body, [100.0, 0.0, 0.0]);
    }
    assert_eq!(physics.damping(flying), (5.0, 0.0));
    run(&mut physics, 0.2);
    let (air, ground) = (physics.velocity(flying)[0], physics.velocity(sliding)[0]);
    assert!((air - 100.0).abs() < 1.0, "damped in the air: {air}");
    assert!((ground - 36.8).abs() < 4.0, "on the ground: {ground}");
}

#[test]
fn an_impact_is_touching_again_after_being_apart() {
    // GMod, measured: a crate lying on the floor lifted 0.55 and sent down at 120 units/s makes
    // no impact; lifted 0.65 and sent down at 5, it makes one, at about the speed it came
    for (lift, down, impact) in [(0.4f32, 120.0f32, false), (0.8, 5.0, true), (0.8, 60.0, true), (2.0, 0.0, true)] {
        let mut physics = floor();
        let body = physics.add(&crate_body([0.0, 0.0, 16.0])).unwrap();
        run(&mut physics, 1.0);
        physics.impacts();
        physics.set_pose(body, [0.0, 0.0, 16.0 + lift], [0.0; 3]);
        physics.set_velocity(body, [0.0, 0.0, -down]);
        run(&mut physics, 0.1);
        let impacts = physics.impacts();
        assert_eq!(impacts.is_empty(), !impact, "lifted {lift}, sent down at {down}: {impacts:?}");
        if let Some(first) = impacts.first() {
            let came = (down * down + 2.0 * DEFAULT_GRAVITY * lift).sqrt();
            assert!((first.speed - came).abs() < 15.0, "{} for {came}", first.speed);
        }
    }
}
