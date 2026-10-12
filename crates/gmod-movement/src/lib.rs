//! How players move: one tick of a player's movement, from what they ask for (the keys held,
//! where they look) through the world as [`Collide`] answers for it.
//!
//! The rules are those Source moves players by, as they show in the game:
//!
//! * On the ground friction slows the player: the speed falls by `max(speed, stop_speed) ×
//!   friction` a second, so that a slow player stops rather than creeps. Then they speed up
//!   toward where they want to go, by `accelerate × the wanted speed` a second, never past the
//!   wanted speed in that direction.
//! * In the air there is no friction, and of the wanted speed at most 30 units a second count
//!   in the direction pushed: enough to steer and to strafe, not to fly.
//! * A move that runs into something slides along it. On the ground a move is also tried a
//!   step higher, and whichever got further is kept: that is how stairs are climbed. Ground
//!   is what faces at least 0.7 up (up to 45.6° steep); steeper is a slope to slide down.
//! * Gravity counts half before the move and half after it, so that a fall or a jump traces
//!   the same arc whatever the tick. A jump needs the key pressed anew; holding it does not
//!   bounce.
//! * Ducking takes `duck_speed` seconds on the ground: the eyes go down, then the box is the
//!   small one. In the air it is at once, the feet pulled up. Standing up needs room.
//! * In water to the waist or deeper there is no gravity: the player swims where they look,
//!   at 0.8 of their speed, sinks slowly when they do nothing and rises while holding jump.
//! * On a ladder - a brush of `CONTENTS_LADDER` right in front of the box, where the player
//!   moves or where the ladder was - there is no gravity either: the player hangs on it, and
//!   moves at 200 units a second where they look, what goes into the ladder turned upward (so
//!   looking down and walking at it goes down). Jump pushes them off it.
//! * Noclip ([`Player::noclip`]) flies where the player looks, through everything: the
//!   speed builds up by `noclip_accelerate` toward `noclip_speed` times the player's speed and
//!   bleeds off by friction, never by less than a quarter of that top speed a second times
//!   the friction, so that letting go stops the player quickly.
//!
//! Not yet: ladders of HL2 that are used (`func_useableladder`), ground that moves, the push
//! of conveyors and triggers, jumping out of water onto a ledge.

#![forbid(unsafe_code)]

use gmod_bsp::{Collision, Trace, contents, mask};

pub type Vec3 = [f32; 3];

/// The world as movement sees it.
pub trait Collide {
    /// Sweeps the box `mins`..`maxs` (around the feet) from `start` to `end`, stopping at
    /// what `mask` names ([`mask`], [`contents`]).
    fn trace_mask(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, mask: u32) -> Trace;
    /// The [`contents`] at a point.
    fn contents(&self, point: Vec3) -> u32;

    /// The same through what stops players.
    fn trace(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Trace {
        self.trace_mask(start, end, mins, maxs, mask::PLAYERSOLID)
    }
}

impl Collide for Collision {
    fn trace_mask(&self, start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3, mask: u32) -> Trace {
        Collision::trace(self, start, end, mins, maxs, mask)
    }

    fn contents(&self, point: Vec3) -> u32 {
        self.point_contents(point)
    }
}

/// The buttons of a command that movement reads (`IN_` in GMod's Lua).
pub mod buttons {
    pub const JUMP: u32 = 2;
    pub const DUCK: u32 = 4;
    /// Run (`+speed`).
    pub const SPEED: u32 = 131072;
}

/// How fast a player climbs a ladder, and how hard jumping pushes them off it.
const CLIMB_SPEED: f32 = 200.0;
const LADDER_JUMP: f32 = 270.0;
/// How far in front of the box a ladder is looked for.
const LADDER_REACH: f32 = 2.0;

/// What a key held asks for: more than any speed, which then caps it (`cl_forwardspeed`).
pub const FULL_MOVE: f32 = 10000.0;

/// Ground is what faces at least this far up.
const GROUND_NORMAL: f32 = 0.7;
/// Going up faster than this, a player has left the ground: they jumped, or were thrown.
const LEAVING_GROUND: f32 = 140.0;
/// How far under the feet ground still counts.
const GROUND_PROBE: f32 = 2.0;
/// The most of the wanted speed that counts in the air.
const AIR_SPEED: f32 = 30.0;
/// Swimming is this much of the speed on land.
const SWIM_SPEED: f32 = 0.8;
/// How fast a player who does nothing in water sinks, and how fast holding jump lifts them.
const SINK_SPEED: f32 = 60.0;
const SWIM_UP_SPEED: f32 = 100.0;
/// A move is cut into at most this many parts by what it runs into.
const BUMPS: usize = 4;
const MAX_PLANES: usize = 5;
/// Less than this along an axis after sliding is nothing.
const STOP_EPSILON: f32 = 0.1;

/// What movement depends on: console variables and the player's class.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// `sv_gravity`: units a second, a second.
    pub gravity: f32,
    /// `sv_friction`.
    pub friction: f32,
    /// `sv_stopspeed`: below this speed friction works as if at it.
    pub stop_speed: f32,
    /// `sv_accelerate`.
    pub accelerate: f32,
    /// `sv_airaccelerate`.
    pub air_accelerate: f32,
    /// `sv_maxvelocity`: no part of the velocity gets past it.
    pub max_velocity: f32,
    /// Units a second walking, and running (`+speed`).
    pub walk_speed: f32,
    pub run_speed: f32,
    /// The speed is multiplied by this while ducking.
    pub crouched_walk_speed: f32,
    /// Seconds to duck, and to stand up.
    pub duck_speed: f32,
    pub unduck_speed: f32,
    /// The upward speed a jump starts with.
    pub jump_power: f32,
    /// The highest step walked up.
    pub step_size: f32,
    /// The box standing and ducked, around the feet.
    pub hull: (Vec3, Vec3),
    pub duck_hull: (Vec3, Vec3),
    /// The eyes over the feet, standing and ducked.
    pub view_height: f32,
    pub duck_view_height: f32,
    /// Noclip goes this many times the player's speed (`sv_noclipspeed`), speeding up by
    /// `sv_noclipaccelerate`.
    pub noclip_speed: f32,
    pub noclip_accelerate: f32,
}

impl Default for Settings {
    /// Garry's Mod's: the console variables as the game sets them, and the player class of
    /// the sandbox (`player_sandbox.lua`) for the speeds.
    fn default() -> Settings {
        Settings {
            gravity: 600.0,
            friction: 8.0,
            stop_speed: 10.0,
            accelerate: 10.0,
            air_accelerate: 10.0,
            max_velocity: 3500.0,
            walk_speed: 200.0,
            run_speed: 400.0,
            crouched_walk_speed: 0.3,
            duck_speed: 0.3,
            unduck_speed: 0.3,
            jump_power: 200.0,
            step_size: 18.0,
            hull: ([-16.0, -16.0, 0.0], [16.0, 16.0, 72.0]),
            duck_hull: ([-16.0, -16.0, 0.0], [16.0, 16.0, 36.0]),
            view_height: 64.0,
            duck_view_height: 28.0,
            noclip_speed: 5.0,
            noclip_accelerate: 5.0,
        }
    }
}

/// What a player asks for in a tick (`CUserCmd`, as `CMoveData` passes it on).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Command {
    /// Where the player looks: pitch, yaw and roll, in degrees.
    pub angles: Vec3,
    /// Units a second forward (back when negative) and to the right (left when negative); the
    /// player's speed caps them, so a key held asks for [`FULL_MOVE`].
    pub forward_move: f32,
    pub side_move: f32,
    /// [`buttons`] held.
    pub buttons: u32,
}

/// A player, as far as movement goes.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Player {
    /// The feet: the middle of the bottom of the box.
    pub origin: Vec3,
    /// Units a second.
    pub velocity: Vec3,
    pub on_ground: bool,
    /// The box is the small one.
    pub ducked: bool,
    /// The eyes over the feet; they move while ducking and standing up.
    pub view_height: f32,
    /// How deep in water: 0 not, 1 to the feet, 2 to the waist, 3 over the eyes.
    pub water_level: u8,
    /// The buttons of the last command.
    pub buttons: u32,
    /// On a ladder: its normal, facing the player.
    pub ladder: Option<Vec3>,
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn length(a: Vec3) -> f32 {
    dot(a, a).sqrt()
}

/// Whether a box fits at `origin` without touching anything.
fn fits(world: &impl Collide, origin: Vec3, (mins, maxs): (Vec3, Vec3)) -> bool {
    !world.trace(origin, origin, mins, maxs).start_solid
}

/// `velocity` with what goes into the plane of `normal` taken away.
fn clip(velocity: Vec3, normal: Vec3) -> Vec3 {
    let mut out = sub(velocity, scale(normal, dot(velocity, normal)));
    for c in &mut out {
        if c.abs() < STOP_EPSILON {
            *c = 0.0;
        }
    }
    // what the rounding left going in
    let into = dot(out, normal);
    if into < 0.0 {
        out = sub(out, scale(normal, into));
    }
    out
}

/// Moves a box along `velocity` for `seconds`, sliding along what it runs into. Returns where
/// it got to and the velocity it has left.
fn slide(world: &impl Collide, origin: Vec3, velocity: Vec3, seconds: f32, (mins, maxs): (Vec3, Vec3)) -> (Vec3, Vec3) {
    let first = velocity;
    let (mut origin, mut velocity, mut before) = (origin, velocity, velocity);
    let mut planes: Vec<Vec3> = Vec::with_capacity(MAX_PLANES);
    let mut left = seconds;
    for _ in 0..BUMPS {
        if velocity == [0.0; 3] {
            break;
        }
        let trace = world.trace(origin, add(origin, scale(velocity, left)), mins, maxs);
        if trace.all_solid {
            return (origin, [0.0; 3]);
        }
        if trace.fraction > 0.0 {
            origin = trace.end;
            before = velocity;
            planes.clear();
        }
        if trace.fraction == 1.0 {
            break;
        }
        left -= left * trace.fraction;
        if planes.len() == MAX_PLANES {
            return (origin, [0.0; 3]);
        }
        planes.push(trace.normal);
        // a velocity along every plane hit, if going along one of them is that
        let along_one = (0..planes.len())
            .map(|i| (i, clip(before, planes[i])))
            .find(|&(i, v)| planes.iter().enumerate().all(|(j, &n)| j == i || dot(v, n) >= 0.0));
        velocity = match along_one {
            Some((_, v)) => v,
            // two planes: along the crease where they meet
            None if planes.len() == 2 => {
                let crease = cross(planes[0], planes[1]);
                let len = length(crease);
                if len < 1e-6 {
                    return (origin, [0.0; 3]);
                }
                let crease = scale(crease, 1.0 / len);
                scale(crease, dot(crease, velocity))
            }
            None => return (origin, [0.0; 3]),
        };
        // never back the way it came: stuck in a corner
        if dot(velocity, first) <= 0.0 {
            return (origin, [0.0; 3]);
        }
    }
    (origin, velocity)
}

impl Player {
    /// A player standing at `origin`, or a little above it when there is no room there: a
    /// spawn point is often on the floor itself, and a box touching the floor counts as inside
    /// it. `None` when there is no room within 64 units up.
    pub fn spawn(world: &impl Collide, settings: &Settings, origin: Vec3) -> Option<Player> {
        let origin = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0]
            .into_iter()
            .map(|up| add(origin, [0.0, 0.0, up]))
            .find(|&o| fits(world, o, settings.hull))?;
        let mut player = Player {
            origin,
            velocity: [0.0; 3],
            on_ground: false,
            ducked: false,
            view_height: settings.view_height,
            water_level: 0,
            buttons: 0,
            ladder: None,
        };
        player.categorize(world, settings);
        Some(player)
    }

    /// The box the player is now, around the feet.
    pub fn hull(&self, settings: &Settings) -> (Vec3, Vec3) {
        if self.ducked { settings.duck_hull } else { settings.hull }
    }

    /// Where the player sees from.
    pub fn eye(&self) -> Vec3 {
        add(self.origin, [0.0, 0.0, self.view_height])
    }

    /// One tick of movement, `seconds` long.
    pub fn tick(&mut self, world: &impl Collide, settings: &Settings, command: &Command, seconds: f32) {
        let pressed = command.buttons & !self.buttons;
        self.buttons = command.buttons;
        self.unstick(world, settings);
        self.categorize(world, settings);
        self.duck(world, settings, command, seconds);
        let speed = self.max_speed(settings, command);
        self.ladder = self.find_ladder(world, settings, command);
        if let Some(normal) = self.ladder {
            self.climb(world, settings, command, pressed, normal, seconds);
        } else if self.water_level >= 2 {
            self.swim(world, settings, command, speed, seconds);
        } else {
            self.walk(world, settings, command, pressed, speed, seconds);
        }
    }

    /// One tick of noclip: flying where the player looks, up and down included, through
    /// everything and without gravity. Running (`+speed`) flies at the running speed.
    pub fn noclip(&mut self, settings: &Settings, command: &Command, seconds: f32) {
        self.buttons = command.buttons;
        self.on_ground = false;
        self.ladder = None;
        let [pitch, yaw, _] = command.angles.map(f32::to_radians);
        let ((sp, cp), (sy, cy)) = (pitch.sin_cos(), yaw.sin_cos());
        let wish = add(scale([cp * cy, cp * sy, -sp], command.forward_move), scale([sy, -cy, 0.0], command.side_move));
        let top = if command.buttons & buttons::SPEED != 0 { settings.run_speed } else { settings.walk_speed } * settings.noclip_speed;
        let len = length(wish);
        let (direction, wanted) = if len > 0.0 { (scale(wish, 1.0 / len), len.min(top)) } else { ([0.0; 3], 0.0) };
        self.accelerate(direction, wanted, settings.noclip_accelerate, seconds);
        let speed = length(self.velocity);
        if speed < 1.0 {
            self.velocity = [0.0; 3];
            return;
        }
        let slower = (speed - speed.max(top / 4.0) * settings.friction * seconds).max(0.0);
        self.velocity = scale(self.velocity, slower / speed);
        self.origin = add(self.origin, scale(self.velocity, seconds));
    }

    /// A player inside something - put there, or left there by a moving brush - is moved to
    /// the nearest place with room around, if there is one close by.
    fn unstick(&mut self, world: &impl Collide, s: &Settings) {
        let hull = self.hull(s);
        if fits(world, self.origin, hull) {
            return;
        }
        // up first; sideways also a little up, off a floor the box may touch as well
        let nudges = [1.0, 2.0, 4.0, 8.0, 16.0].into_iter().flat_map(|d| {
            [
                [0.0, 0.0, d],
                [d, 0.0, 0.0],
                [-d, 0.0, 0.0],
                [0.0, d, 0.0],
                [0.0, -d, 0.0],
                [d, 0.0, 1.0],
                [-d, 0.0, 1.0],
                [0.0, d, 1.0],
                [0.0, -d, 1.0],
                [0.0, 0.0, -d],
            ]
        });
        if let Some(free) = nudges.map(|nudge| add(self.origin, nudge)).find(|&o| fits(world, o, hull)) {
            self.origin = free;
        }
    }

    /// A ladder right in front of the box: where the player moves, or where the ladder was.
    /// Returns its normal.
    fn find_ladder(&self, world: &impl Collide, s: &Settings, command: &Command) -> Option<Vec3> {
        let (mins, maxs) = self.hull(s);
        let (sin, cos) = command.angles[1].to_radians().sin_cos();
        let wish = add(scale([cos, sin, 0.0], command.forward_move), scale([sin, -cos, 0.0], command.side_move));
        let wish = (length(wish) > 0.0).then(|| scale(wish, 1.0 / length(wish)));
        // the ladder the player was on keeps them, unless they walk off it on the ground
        let back = self.ladder.filter(|&n| !(self.on_ground && wish.is_some_and(|w| dot(w, n) > 0.0))).map(|n| scale(n, -1.0));
        for direction in wish.into_iter().chain(back) {
            let probe = world.trace_mask(self.origin, add(self.origin, scale(direction, LADDER_REACH)), mins, maxs, contents::LADDER);
            // inside a ladder that is not solid (func_ladder): it faces back along the probe
            if probe.start_solid {
                return Some(self.ladder.unwrap_or(scale(direction, -1.0)));
            }
            if probe.fraction < 1.0 {
                return Some(probe.normal);
            }
        }
        None
    }

    /// A tick on a ladder: no gravity, moving where the player looks, into the ladder upward.
    fn climb(&mut self, world: &impl Collide, s: &Settings, command: &Command, pressed: u32, normal: Vec3, seconds: f32) {
        self.on_ground = false;
        if pressed & buttons::JUMP != 0 {
            self.velocity = scale(normal, LADDER_JUMP);
            self.ladder = None;
        } else {
            let [pitch, yaw, _] = command.angles.map(f32::to_radians);
            let ((sp, cp), (sy, cy)) = (pitch.sin_cos(), yaw.sin_cos());
            let wish = add(scale([cp * cy, cp * sy, -sp], command.forward_move), scale([sy, -cy, 0.0], command.side_move));
            let len = length(wish);
            let wish = if len > 0.0 { scale(wish, CLIMB_SPEED / len) } else { [0.0; 3] };
            let into = dot(wish, normal);
            let along = sub(wish, scale(normal, into));
            // up the ladder's face
            let up = sub([0.0, 0.0, 1.0], scale(normal, normal[2]));
            let up = if length(up) > 1e-3 { scale(up, 1.0 / length(up)) } else { [0.0, 0.0, 1.0] };
            self.velocity = sub(along, scale(up, into));
        }
        (self.origin, self.velocity) = slide(world, self.origin, self.velocity, seconds, self.hull(s));
        self.categorize(world, s);
        if self.on_ground && self.velocity[2] < 0.0 {
            self.velocity[2] = 0.0;
        }
    }

    /// How fast the player may go.
    fn max_speed(&self, s: &Settings, command: &Command) -> f32 {
        let speed = if command.buttons & buttons::SPEED != 0 { s.run_speed } else { s.walk_speed };
        let ducking = self.ducked || (command.buttons & buttons::DUCK != 0 && self.on_ground);
        if ducking { speed * s.crouched_walk_speed } else { speed }
    }

    /// The water level, and whether there is ground under the feet.
    fn categorize(&mut self, world: &impl Collide, s: &Settings) {
        let (mins, maxs) = self.hull(s);
        let wet = |z: f32| world.contents(add(self.origin, [0.0, 0.0, z])) & (contents::WATER | contents::SLIME) != 0;
        self.water_level = if !wet(mins[2] + 1.0) {
            0
        } else if !wet((mins[2] + maxs[2]) / 2.0) {
            1
        } else if !wet(self.view_height) {
            2
        } else {
            3
        };
        if self.velocity[2] > LEAVING_GROUND {
            self.on_ground = false;
            return;
        }
        let below = world.trace(self.origin, sub(self.origin, [0.0, 0.0, GROUND_PROBE]), mins, maxs);
        self.on_ground = below.fraction < 1.0 && below.normal[2] >= GROUND_NORMAL;
        // standing, the feet are on the ground rather than over it
        if self.on_ground && !below.start_solid {
            self.origin = below.end;
        }
    }

    fn duck(&mut self, world: &impl Collide, s: &Settings, command: &Command, seconds: f32) {
        let lift = s.hull.1[2] - s.duck_hull.1[2];
        let span = s.view_height - s.duck_view_height;
        let want = command.buttons & buttons::DUCK != 0;
        match (want, self.ducked, self.on_ground) {
            // the eyes go down, and then the box is the small one
            (true, false, true) => {
                self.view_height = (self.view_height - span * seconds / s.duck_speed.max(1e-3)).max(s.duck_view_height);
                self.ducked = self.view_height <= s.duck_view_height;
            }
            // in the air at once: the feet come up and the head stays, if there is room above
            (true, false, false) => {
                let raised = add(self.origin, [0.0, 0.0, lift]);
                if fits(world, raised, s.duck_hull) {
                    self.origin = raised;
                }
                self.ducked = true;
                self.view_height = s.duck_view_height;
            }
            // the eyes come up while there is room to stand, and then the box is the big one
            (false, true, true) => {
                if fits(world, self.origin, s.hull) {
                    self.view_height = (self.view_height + span * seconds / s.unduck_speed.max(1e-3)).min(s.view_height);
                    self.ducked = self.view_height < s.view_height;
                }
            }
            // in the air the feet go down again, if there is room below
            (false, true, false) => {
                let lowered = sub(self.origin, [0.0, 0.0, lift]);
                if fits(world, lowered, s.hull) {
                    self.origin = lowered;
                    self.ducked = false;
                    self.view_height = s.view_height;
                }
            }
            // let go before the duck was done
            (false, false, _) if self.view_height < s.view_height => {
                self.view_height = (self.view_height + span * seconds / s.unduck_speed.max(1e-3)).min(s.view_height);
            }
            _ => {}
        }
    }

    fn walk(&mut self, world: &impl Collide, s: &Settings, command: &Command, pressed: u32, speed: f32, seconds: f32) {
        // a jump starts before friction: landing and jumping at once loses no speed
        if pressed & buttons::JUMP != 0 && self.on_ground {
            self.velocity[2] = s.jump_power;
            self.on_ground = false;
        }
        if !self.on_ground {
            self.velocity[2] -= s.gravity * seconds / 2.0;
        }
        if self.on_ground {
            self.velocity[2] = 0.0;
            self.friction(s, seconds);
        }
        self.clamp(s);
        let [_, yaw, _] = command.angles.map(f32::to_radians);
        let (sin, cos) = yaw.sin_cos();
        let wish = add(scale([cos, sin, 0.0], command.forward_move), scale([sin, -cos, 0.0], command.side_move));
        let len = length(wish);
        let (direction, wanted) = if len > 0.0 { (scale(wish, 1.0 / len), len.min(speed)) } else { ([0.0; 3], 0.0) };
        if self.on_ground {
            self.accelerate(direction, wanted, s.accelerate, seconds);
            self.velocity[2] = 0.0;
            self.walk_move(world, s, seconds);
        } else {
            self.air_accelerate(direction, wanted, s.air_accelerate, seconds);
            (self.origin, self.velocity) = slide(world, self.origin, self.velocity, seconds, self.hull(s));
        }
        self.categorize(world, s);
        self.clamp(s);
        if self.on_ground {
            self.velocity[2] = 0.0;
        } else {
            self.velocity[2] -= s.gravity * seconds / 2.0;
        }
    }

    fn swim(&mut self, world: &impl Collide, s: &Settings, command: &Command, speed: f32, seconds: f32) {
        if command.buttons & buttons::JUMP != 0 {
            self.velocity[2] = SWIM_UP_SPEED;
        }
        // where the player looks, up and down included
        let [pitch, yaw, _] = command.angles.map(f32::to_radians);
        let ((sp, cp), (sy, cy)) = (pitch.sin_cos(), yaw.sin_cos());
        let mut wish = add(scale([cp * cy, cp * sy, -sp], command.forward_move), scale([sy, -cy, 0.0], command.side_move));
        if command.forward_move == 0.0 && command.side_move == 0.0 {
            wish[2] -= SINK_SPEED;
        }
        let len = length(wish);
        let wanted = len.min(speed) * SWIM_SPEED;
        // water slows everything
        let current = length(self.velocity);
        if current > 0.0 {
            let slower = (current - seconds * current * s.friction).max(0.0);
            self.velocity = scale(self.velocity, slower / current);
        }
        if wanted >= 0.1 {
            let more = wanted - length(self.velocity);
            if more > 0.0 {
                let gain = (s.accelerate * wanted * seconds).min(more);
                self.velocity = add(self.velocity, scale(wish, gain / len));
            }
        }
        (self.origin, self.velocity) = slide(world, self.origin, self.velocity, seconds, self.hull(s));
        self.categorize(world, s);
        if self.on_ground {
            self.velocity[2] = 0.0;
        }
    }

    fn friction(&mut self, s: &Settings, seconds: f32) {
        let speed = length(self.velocity);
        if speed < 0.1 {
            return;
        }
        let slower = (speed - speed.max(s.stop_speed) * s.friction * seconds).max(0.0);
        self.velocity = scale(self.velocity, slower / speed);
    }

    fn accelerate(&mut self, direction: Vec3, wanted: f32, accelerate: f32, seconds: f32) {
        let more = wanted - dot(self.velocity, direction);
        if more > 0.0 {
            self.velocity = add(self.velocity, scale(direction, (accelerate * seconds * wanted).min(more)));
        }
    }

    fn air_accelerate(&mut self, direction: Vec3, wanted: f32, accelerate: f32, seconds: f32) {
        let more = wanted.min(AIR_SPEED) - dot(self.velocity, direction);
        if more > 0.0 {
            self.velocity = add(self.velocity, scale(direction, (accelerate * wanted * seconds).min(more)));
        }
    }

    fn clamp(&mut self, s: &Settings) {
        self.velocity = self.velocity.map(|v| v.clamp(-s.max_velocity, s.max_velocity));
    }

    /// A move on the ground: straight there if nothing is in the way, else sliding or up a step.
    fn walk_move(&mut self, world: &impl Collide, s: &Settings, seconds: f32) {
        if length(self.velocity) < 1.0 {
            self.velocity = [0.0; 3];
            return;
        }
        let (mins, maxs) = self.hull(s);
        let straight = world.trace(self.origin, add(self.origin, scale(self.velocity, seconds)), mins, maxs);
        if straight.fraction == 1.0 {
            self.origin = straight.end;
        } else {
            self.step_move(world, s, seconds);
        }
        self.stay_on_ground(world, s);
    }

    /// Slides on, and tries the same a step higher; keeps whichever got further.
    fn step_move(&mut self, world: &impl Collide, s: &Settings, seconds: f32) {
        let hull = self.hull(s);
        let (start, velocity) = (self.origin, self.velocity);
        let (low, low_velocity) = slide(world, start, velocity, seconds, hull);
        let raised = world.trace(start, add(start, [0.0, 0.0, s.step_size]), hull.0, hull.1);
        let (high, high_velocity) = slide(world, raised.end, velocity, seconds, hull);
        let down = world.trace(high, sub(high, [0.0, 0.0, s.step_size]), hull.0, hull.1);
        // a step onto what is too steep to stand on is no step
        if raised.all_solid || down.all_solid || (down.fraction < 1.0 && down.normal[2] < GROUND_NORMAL) {
            (self.origin, self.velocity) = (low, low_velocity);
            return;
        }
        let flat = |p: Vec3| (p[0] - start[0]).hypot(p[1] - start[1]);
        if flat(low) >= flat(down.end) {
            (self.origin, self.velocity) = (low, low_velocity);
        } else {
            self.origin = down.end;
            self.velocity = [high_velocity[0], high_velocity[1], low_velocity[2]];
        }
    }

    /// Keeps a player walking down stairs and slopes on them instead of off into the air.
    fn stay_on_ground(&mut self, world: &impl Collide, s: &Settings) {
        let (mins, maxs) = self.hull(s);
        let up = world.trace(self.origin, add(self.origin, [0.0, 0.0, GROUND_PROBE]), mins, maxs);
        let down = world.trace(up.end, sub(self.origin, [0.0, 0.0, s.step_size]), mins, maxs);
        if down.fraction > 0.0 && down.fraction < 1.0 && !down.start_solid && down.normal[2] >= GROUND_NORMAL {
            self.origin = down.end;
        }
    }
}

#[cfg(test)]
mod tests;
