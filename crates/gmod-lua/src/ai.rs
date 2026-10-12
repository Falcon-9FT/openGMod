//! What the engine's NPCs do by themselves each tick ([`crate::npc::tick`]): their schedules.
//!
//! An NPC runs a schedule - one Lua set (`SetSchedule`), or one it chose - until it is done,
//! then chooses another. With no enemy it stands (`SCHED_IDLE_STAND`). With one, an NPC with a
//! gun shoots it in bursts where it sees it within the gun's range (`SCHED_RANGE_ATTACK1`),
//! reloads when the clip is out (`SCHED_RELOAD`) and else goes after it (`SCHED_CHASE_ENEMY`);
//! one that bites or claws - zombies, headcrabs, antlions, manhacks, one with a stunstick -
//! goes after it and strikes within reach (`SCHED_MELEE_ATTACK1`); one that does neither, or
//! cannot get to it, faces it (`SCHED_COMBAT_FACE`). Standing, wandering and facing give way to
//! an enemy, and going after one to striking it. Of what Lua sets, these run: going to the last
//! position (`SetLastPosition` and `SCHED_FORCED_GO`, `SCHED_FORCED_GO_RUN`), wandering and
//! patrolling (`SCHED_IDLE_WANDER`, `SCHED_IDLE_WALK`, `SCHED_PATROL_WALK`, `SCHED_PATROL_RUN`,
//! `SCHED_RUN_RANDOM`), going after the target (`SCHED_TARGET_CHASE`) or facing it
//! (`SCHED_TARGET_FACE`), getting away from the enemy (`SCHED_RUN_FROM_ENEMY`,
//! `SCHED_TAKE_COVER_FROM_ENEMY`, `SCHED_MOVE_AWAY_FROM_ENEMY`), standing still
//! (`SCHED_NPC_FREEZE`); any other stands.
//!
//! Moving, an NPC follows a way of the node graph ([`crate::nav`]) at the speed its walk or run
//! carries its model (a speed of our own when the model does not say), playing that activity
//! (`ACT_WALK`, `ACT_RUN`, the walk for a model that does not run, or the movement activity Lua
//! sets), up and down steps, sliding along what it meets and facing where it goes; one that
//! does not get on for a second gives up. The way to something that moves is found again as
//! it moves. Lua sets goals of its own (`NavSetGoal`, `NavSetGoalTarget`, `NavSetWanderGoal`...),
//! which a standing NPC walks to, and stops them (`StopMoving`).
//!
//! An NPC fires the engine's guns as `FireBullets` from its eyes at its enemy's middle, within
//! the gun's cone widened by its proficiency, with the gun's `single_shot_npc` sound and the
//! NPC damage of the ammo type (`sk_npc_dmg_*`); it does not fire through a friend. A SWEP is
//! asked how it bursts and rests (`GetNPCBurstSettings`, `GetNPCRestTimes`) and fires by
//! `NPCShoot_Primary`. A strike lands a moment after the swing starts, if the enemy is still in
//! reach and before it, of the class's skill ConVar's damage (`sk_zombie_dmg_one_slash`...).
//!
//! How fast NPCs walk and run when their models do not say, how far they shoot, their bursts
//! and rests, how a proficiency spreads their shots, their reach and the damage of those
//! without a skill ConVar are our own choices, near what the game does.

use mlua::{Lua, MultiValue, Value};

use crate::bullets::{self, Bullets};
use crate::damage::{self, DamageInfo};
use crate::entity::{self, EntityHandle, Kind};
use crate::host;
use crate::nav::{self, Hull, Solid, Stop};
use crate::npc;
use crate::vector::Vector;

/// Schedules (`SCHED_`).
pub(crate) mod sched {
    pub const IDLE_STAND: i64 = 1;
    pub const IDLE_WALK: i64 = 2;
    pub const IDLE_WANDER: i64 = 3;
    pub const COMBAT_FACE: i64 = 12;
    pub const CHASE_ENEMY: i64 = 17;
    pub const TARGET_FACE: i64 = 20;
    pub const TARGET_CHASE: i64 = 21;
    pub const MOVE_AWAY_FROM_ENEMY: i64 = 25;
    pub const TAKE_COVER_FROM_ENEMY: i64 = 27;
    pub const RUN_FROM_ENEMY: i64 = 32;
    pub const MELEE_ATTACK1: i64 = 41;
    pub const RANGE_ATTACK1: i64 = 43;
    pub const RELOAD: i64 = 51;
    pub const FORCED_GO: i64 = 71;
    pub const FORCED_GO_RUN: i64 = 72;
    pub const NPC_FREEZE: i64 = 73;
    pub const PATROL_WALK: i64 = 74;
    pub const PATROL_RUN: i64 = 76;
    pub const RUN_RANDOM: i64 = 77;
}

pub(crate) const ACT_IDLE: i64 = 1;
pub(crate) const ACT_WALK: i64 = 6;
pub(crate) const ACT_RUN: i64 = 10;
const ACT_RANGE_ATTACK1: i64 = 16;
const ACT_MELEE_ATTACK1: i64 = 64;
const ACT_RELOAD: i64 = 66;
const ACT_IDLE_ANGRY: i64 = 76;

const DMG_SLASH: i64 = 4;
const DMG_CLUB: i64 = 128;
const D_HT: i64 = 1;
const D_LI: i64 = 3;
const MOVETYPE_STEP: i64 = 3;
const MOVETYPE_FLY: i64 = 4;
const MOVETYPE_FLYGRAVITY: i64 = 5;

/// How fast an NPC walks, runs and flies when its model does not say.
const WALK_SPEED: f32 = 75.0;
const RUN_SPEED: f32 = 180.0;
const FLY_SPEED: f32 = 250.0;
/// How long an NPC that does not get on tries before it gives up.
const STALL_TIME: f64 = 1.0;
/// How often a way to something that moves is found again.
const REROUTE: f64 = 0.5;
/// How near a point and a target an NPC goes to have got there.
const POINT_TOLERANCE: f32 = 12.0;
const TARGET_TOLERANCE: f32 = 64.0;
/// How near its enemy an NPC with a gun goes when it cannot shoot it.
const GUN_CHASE_TOLERANCE: f32 = 128.0;
/// How far off its enemy an NPC may face and still fire, and still swing, in degrees.
const AIM_CONE: f32 = 30.0;
const SWING_CONE: f32 = 60.0;
/// How long a reload takes when the model does not play one.
const RELOAD_TIME: f64 = 1.5;
/// How far a wander goes, and how long a patrol rests at each end.
const WANDER: (f32, f32) = (128.0, 512.0);
const PATROL_REST: (f64, f64) = (1.0, 3.0);
/// How far an NPC runs from its enemy.
const FLEE: (f32, f32) = (256.0, 768.0);
/// How long a goal it could not reach is not tried again.
const UNREACHABLE_TIME: f64 = 3.0;
/// The degrees a proficiency (`WEAPON_PROFICIENCY_POOR` to `_PERFECT`) adds to a gun's cone.
const PROFICIENCY_CONE: [f32; 5] = [12.0, 8.0, 5.0, 3.0, 1.0];

/// A gun of the engine in an NPC's hands.
pub(crate) struct NpcGun {
    class: &'static str,
    /// Seconds between the shots of a burst.
    interval: f64,
    /// How many shots a burst has, at least and at most.
    burst: (u32, u32),
    /// How long it rests between bursts, at least and at most.
    rest: (f64, f64),
    /// The cone its bullets go in, in degrees across, before the proficiency's.
    cone: f32,
    pellets: u32,
    range: f32,
    /// The activity of the NPC firing it, before `ACT_RANGE_ATTACK1`.
    act: &'static str,
}

#[rustfmt::skip]
const NPC_GUNS: &[NpcGun] = &[
    NpcGun { class: "weapon_pistol", interval: 0.3, burst: (1, 3), rest: (0.5, 1.0), cone: 1.0, pellets: 1, range: 1500.0, act: "ACT_RANGE_ATTACK_PISTOL" },
    NpcGun { class: "weapon_smg1", interval: 0.1, burst: (3, 6), rest: (0.6, 1.2), cone: 5.0, pellets: 1, range: 1500.0, act: "ACT_RANGE_ATTACK_SMG1" },
    NpcGun { class: "weapon_ar2", interval: 0.1, burst: (2, 5), rest: (0.6, 1.2), cone: 3.0, pellets: 1, range: 2000.0, act: "ACT_RANGE_ATTACK_AR2" },
    NpcGun { class: "weapon_shotgun", interval: 0.8, burst: (1, 1), rest: (0.4, 0.8), cone: 10.0, pellets: 8, range: 600.0, act: "ACT_RANGE_ATTACK_SHOTGUN" },
    NpcGun { class: "weapon_357", interval: 0.75, burst: (1, 1), rest: (0.8, 1.4), cone: 0.0, pellets: 1, range: 2000.0, act: "ACT_RANGE_ATTACK_PISTOL" },
    NpcGun { class: "weapon_alyxgun", interval: 0.1, burst: (3, 5), rest: (0.5, 1.0), cone: 2.0, pellets: 1, range: 1500.0, act: "ACT_RANGE_ATTACK_PISTOL" },
    NpcGun { class: "weapon_annabelle", interval: 0.75, burst: (1, 1), rest: (1.0, 1.5), cone: 0.0, pellets: 1, range: 2000.0, act: "ACT_RANGE_ATTACK_SHOTGUN" },
];

/// A strike of an NPC of a class, or of one holding a weapon of a class.
pub(crate) struct Melee {
    class: &'static str,
    /// How far past the middle of its enemy's box it reaches from its own middle.
    range: f32,
    /// The skill ConVar of its damage, and the damage without one.
    skill: &'static str,
    damage: f32,
    /// Seconds from one swing to the next, and from a swing to its landing.
    interval: f64,
    delay: f64,
    kind: i64,
}

#[rustfmt::skip]
const MELEE: &[Melee] = &[
    Melee { class: "npc_zombie", range: 55.0, skill: "sk_zombie_dmg_one_slash", damage: 10.0, interval: 1.2, delay: 0.5, kind: DMG_SLASH },
    Melee { class: "npc_zombie_torso", range: 45.0, skill: "sk_zombie_dmg_one_slash", damage: 10.0, interval: 1.2, delay: 0.5, kind: DMG_SLASH },
    Melee { class: "npc_zombine", range: 55.0, skill: "sk_zombie_dmg_one_slash", damage: 10.0, interval: 1.2, delay: 0.5, kind: DMG_SLASH },
    Melee { class: "npc_poisonzombie", range: 60.0, skill: "", damage: 20.0, interval: 1.5, delay: 0.6, kind: DMG_SLASH },
    Melee { class: "npc_fastzombie", range: 55.0, skill: "", damage: 6.0, interval: 0.5, delay: 0.2, kind: DMG_SLASH },
    Melee { class: "npc_fastzombie_torso", range: 45.0, skill: "", damage: 6.0, interval: 0.5, delay: 0.2, kind: DMG_SLASH },
    Melee { class: "npc_headcrab", range: 80.0, skill: "sk_headcrab_melee_dmg", damage: 5.0, interval: 1.5, delay: 0.4, kind: DMG_SLASH },
    Melee { class: "npc_headcrab_fast", range: 80.0, skill: "sk_headcrab_melee_dmg", damage: 5.0, interval: 1.0, delay: 0.3, kind: DMG_SLASH },
    Melee { class: "npc_headcrab_black", range: 80.0, skill: "", damage: 10.0, interval: 2.0, delay: 0.4, kind: DMG_SLASH },
    Melee { class: "npc_headcrab_poison", range: 80.0, skill: "", damage: 10.0, interval: 2.0, delay: 0.4, kind: DMG_SLASH },
    Melee { class: "npc_antlion", range: 55.0, skill: "sk_antlion_swipe_damage", damage: 5.0, interval: 0.8, delay: 0.3, kind: DMG_SLASH },
    Melee { class: "npc_antlion_worker", range: 55.0, skill: "sk_antlion_swipe_damage", damage: 5.0, interval: 0.8, delay: 0.3, kind: DMG_SLASH },
    Melee { class: "npc_antlionguard", range: 100.0, skill: "sk_antlionguard_dmg_shove", damage: 10.0, interval: 1.5, delay: 0.5, kind: DMG_CLUB },
    Melee { class: "npc_hunter", range: 70.0, skill: "sk_hunter_dmg_one_slash", damage: 20.0, interval: 1.2, delay: 0.4, kind: DMG_SLASH },
    Melee { class: "npc_vortigaunt", range: 64.0, skill: "sk_vortigaunt_dmg_claw", damage: 10.0, interval: 1.0, delay: 0.4, kind: DMG_SLASH },
    Melee { class: "npc_manhack", range: 40.0, skill: "sk_manhack_melee_dmg", damage: 20.0, interval: 0.5, delay: 0.0, kind: DMG_SLASH },
    Melee { class: "npc_dog", range: 80.0, skill: "", damage: 20.0, interval: 1.2, delay: 0.4, kind: DMG_CLUB },
    Melee { class: "weapon_crowbar", range: 64.0, skill: "", damage: 10.0, interval: 1.0, delay: 0.3, kind: DMG_CLUB },
    Melee { class: "weapon_stunstick", range: 64.0, skill: "", damage: 40.0, interval: 1.0, delay: 0.3, kind: DMG_CLUB },
];

/// Where an NPC is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Goal {
    /// A point, when there is no target.
    pub pos: Vector,
    /// What it goes after, from where it is (`NULL` for a point).
    pub target: EntityHandle,
    pub offset: Vector,
    /// How near it goes to have got there.
    pub tolerance: f32,
    pub run: bool,
}

impl Goal {
    pub fn point(pos: Vector, tolerance: f32, run: bool) -> Goal {
        Goal { pos, target: EntityHandle::NULL, offset: Vector::default(), tolerance, run }
    }

    pub fn after(target: EntityHandle, offset: Vector, tolerance: f32, run: bool) -> Goal {
        Goal { pos: Vector::default(), target, offset, tolerance, run }
    }
}

/// What an NPC is doing.
#[derive(Debug, Clone, Default)]
pub(crate) struct Mind {
    /// The schedule that was started: another (or the same set again) starts anew.
    pub started: Option<i64>,
    pub goal: Option<Goal>,
    /// The points to go through, the goal's last.
    pub route: Vec<Vector>,
    /// How it moved last tick.
    pub velocity: Vector,
    /// How long it has not got on.
    stalled: f64,
    /// When the way to a target that moves is found again.
    next_route: f64,
    /// What stood in its way last.
    pub blocker: EntityHandle,
    /// Until when a patrol rests.
    rest_until: f64,
    /// When it fires or swings next, and the shots left of a burst.
    next_attack: f64,
    burst: u32,
    /// A swing that lands then, on that.
    swing: Option<(f64, EntityHandle)>,
    /// When a reload is done.
    reload_until: Option<f64>,
    /// How near its next goal Lua says it goes (`SetArrivalDistance`).
    pub arrival_distance: Option<f32>,
    /// What it could not get to, until when.
    pub unreachable: Vec<(EntityHandle, f64)>,
    /// The navigation type Lua set (`SetNavType`).
    pub nav_type: Option<i64>,
}

fn now(lua: &Lua) -> f64 {
    host(lua).clock.cur_time.get()
}

fn pos(lua: &Lua, handle: EntityHandle) -> Option<Vector> {
    host(lua).entities.transform(handle).map(|(p, _)| p)
}

fn valid(lua: &Lua, handle: EntityHandle) -> bool {
    host(lua).entities.is_valid(handle)
}

fn flat(v: Vector) -> Vector {
    Vector::new(v.x, v.y, 0.0)
}

/// The middle of an entity's box: where an NPC aims and reaches.
fn middle(lua: &Lua, handle: EntityHandle) -> Option<Vector> {
    let (p, _) = host(lua).entities.transform(handle)?;
    let (mins, maxs) = host(lua).entities.get(handle).map(|e| (e.mins, e.maxs))?;
    Some(p + (mins + maxs) * 0.5)
}

/// A roll from `low` to `high`, of the AI's dice.
fn roll(lua: &Lua, low: f64, high: f64) -> f64 {
    low + (high - low) * (f64::from(bullets::dice(lua)) + 0.5)
}

/// An activity by its `ACT_` name, as the game's enums have it (-1 without).
fn act_named(lua: &Lua, name: &str) -> i64 {
    match lua.globals().raw_get::<Value>(name) {
        Ok(Value::Integer(n)) => n,
        Ok(Value::Number(n)) => n as i64,
        _ => -1,
    }
}

/// The box an NPC moves as, and whether it flies; none for one that does not move.
fn hull_of(lua: &Lua, handle: EntityHandle) -> Option<Hull> {
    let (move_type, mins, maxs) = host(lua).entities.get(handle).map(|e| (e.move_type, e.mins, e.maxs))?;
    let air = match move_type {
        MOVETYPE_STEP => false,
        MOVETYPE_FLY | MOVETYPE_FLYGRAVITY => true,
        _ => return None,
    };
    Some(Hull { mins, maxs, air })
}

/// Plays the first activity of these the model has (the first when it has none), unless the
/// NPC plays it already.
fn ensure_activity(lua: &Lua, handle: EntityHandle, wanted: &[i64]) {
    let wanted: Vec<i64> = wanted.iter().copied().filter(|&a| a >= 0).collect();
    if wanted.is_empty() {
        return;
    }
    let model = host(lua).entities.get(handle).map(|e| e.model.clone()).unwrap_or_default();
    let rig = crate::anim::rig(lua, &model);
    let act = rig.and_then(|r| wanted.iter().copied().find(|&a| crate::anim::has_activity(lua, &r, a))).unwrap_or(wanted[0]);
    if host(lua).entities.get(handle).is_some_and(|e| e.anim.activity == act) {
        return;
    }
    npc::play_activity(lua, handle, act);
}

/// Faces the point at the NPC's speed of turning; whether it faces it within `cone` degrees.
fn face(lua: &Lua, handle: EntityHandle, point: Vector, cone: f32) -> bool {
    npc::turn_towards(lua, handle, point, crate::timer::TICK_INTERVAL as f32);
    facing(lua, handle, point, cone)
}

/// Whether an NPC faces a point within `cone` degrees, about its yaw.
fn facing(lua: &Lua, handle: EntityHandle, point: Vector, cone: f32) -> bool {
    let Some((at, angles)) = host(lua).entities.transform(handle) else { return false };
    let to = point - at;
    if to.x.abs() < 0.01 && to.y.abs() < 0.01 {
        return true;
    }
    let off = (to.y.atan2(to.x).to_degrees() - angles.y + 540.0).rem_euclid(360.0) - 180.0;
    off.abs() <= cone
}

// --- moving ---

/// Where a goal is now: for one that walks, on the floor below it.
fn goal_point(lua: &Lua, goal: &Goal, hull: &Hull) -> Option<Vector> {
    let point = match goal.target.index {
        Some(_) if hull.air => middle(lua, goal.target)? + goal.offset,
        Some(_) => pos(lua, goal.target)? + goal.offset,
        None => goal.pos,
    };
    if hull.air {
        return Some(point);
    }
    // a point up in the air is gone to on the floor below it
    Some(match nav::floor_below(lua, point + Vector::new(0.0, 0.0, 1.0)) {
        Some(floor) if point.z - floor.z > nav::STEP => floor,
        _ => point,
    })
}

/// Whether an NPC at `at` has got to `to`.
fn arrived(at: Vector, to: Vector, tolerance: f32, hull: &Hull) -> bool {
    if hull.air {
        (to - at).length() <= tolerance
    } else {
        flat(to - at).length() <= tolerance && (to.z - at.z).abs() <= (hull.maxs.z - hull.mins.z).max(nav::STEP * 2.0)
    }
}

/// Sets an NPC out for a goal: the way there, and the activity it goes with. Whether there is a
/// way; with none it stays as it was.
pub(crate) fn set_goal(lua: &Lua, handle: EntityHandle, goal: Goal) -> bool {
    let (Some(hull), Some(from)) = (hull_of(lua, handle), pos(lua, handle)) else { return false };
    let Some(to) = goal_point(lua, &goal, &hull) else { return false };
    let route = if arrived(from, to, goal.tolerance, &hull) { Some(Vec::new()) } else { nav::route(lua, from, to, &hull) };
    let Some(route) = route else { return false };
    let now = now(lua);
    npc::with_npc(lua, handle, |_, npc| {
        let mind = &mut npc.mind;
        let goal = Goal { tolerance: mind.arrival_distance.take().unwrap_or(goal.tolerance), ..goal };
        mind.goal = Some(goal);
        mind.route = route;
        mind.stalled = 0.0;
        mind.next_route = now + REROUTE;
        mind.blocker = EntityHandle::NULL;
        npc.movement_activity = if goal.run { ACT_RUN } else { ACT_WALK };
    });
    true
}

/// The NPC stops where it is; it stands at its next tick ([`think`]).
pub(crate) fn stop(lua: &Lua, handle: EntityHandle) {
    npc::with_npc(lua, handle, |ent, npc| {
        npc.mind.goal = None;
        npc.mind.route.clear();
        npc.mind.velocity = Vector::default();
        ent.velocity = Vector::default();
    });
}

/// How fast an NPC goes: as what it plays carries its model, else as it walks or runs.
pub(crate) fn speed(lua: &Lua, handle: EntityHandle) -> f32 {
    let Some(hull) = hull_of(lua, handle) else { return 0.0 };
    if hull.air {
        return FLY_SPEED;
    }
    let carried = crate::anim::ground_speed(lua, handle);
    if carried > 1.0 {
        return carried;
    }
    let running = host(lua).entities.get(handle).is_some_and(|e| e.anim.activity == ACT_RUN);
    if running { RUN_SPEED } else { WALK_SPEED }
}

/// How far an NPC has left to go along its way.
pub(crate) fn path_length(lua: &Lua, handle: EntityHandle) -> f32 {
    let Some(at) = pos(lua, handle) else { return 0.0 };
    let route = npc::with_npc(lua, handle, |_, npc| npc.mind.route.clone()).unwrap_or_default();
    let mut from = at;
    let mut total = 0.0;
    for point in route {
        total += (point - from).length();
        from = point;
    }
    total
}

/// One step of a walk or a flight across `delta`, sliding along what is in the way.
fn go(lua: &Lua, at: Vector, delta: Vector, hull: &Hull, solid: &Solid) -> Result<Vector, (Stop, EntityHandle)> {
    let once = |delta: Vector| -> Result<Vector, (Stop, EntityHandle)> {
        if hull.air {
            let hit = nav::sweep(lua, at, at + delta, Some(hull), solid);
            if hit.start_solid || hit.fraction < 1.0 {
                return Err((Stop::Blocked(hit.normal), hit.entity));
            }
            Ok(hit.end)
        } else {
            nav::stride(lua, at, delta, hull, solid).map_err(|stop| {
                let blocker = match stop {
                    Stop::Blocked(_) => {
                        nav::sweep(
                            lua,
                            at + Vector::new(0.0, 0.0, nav::STEP),
                            at + Vector::new(0.0, 0.0, nav::STEP) + delta,
                            Some(hull),
                            solid,
                        )
                        .entity
                    }
                    Stop::Ledge => EntityHandle::NULL,
                };
                (stop, blocker)
            })
        }
    };
    match once(delta) {
        Err((Stop::Blocked(normal), blocker)) => {
            // along the wall, what is left of the step
            let normal = if hull.air { normal } else { flat(normal) };
            let slid = delta - normal * delta.dot(normal);
            let slid = if hull.air { slid } else { flat(slid) };
            if slid.length() < 0.1 || normal.length() < 0.01 {
                return Err((Stop::Blocked(normal), blocker));
            }
            once(slid).map_err(|(stop, _)| (stop, blocker))
        }
        other => other,
    }
}

/// What a step of moving came to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Progress {
    Moving,
    Arrived,
    Failed,
}

/// An NPC goes on along its way for a tick.
fn move_on(lua: &Lua, handle: EntityHandle, dt: f64) -> Progress {
    let Some((Some(goal), route, next_route)) =
        npc::with_npc(lua, handle, |_, npc| (npc.mind.goal, npc.mind.route.clone(), npc.mind.next_route))
    else {
        return Progress::Arrived;
    };
    let (Some(hull), Some(start)) = (hull_of(lua, handle), pos(lua, handle)) else { return Progress::Failed };
    if goal.target.index.is_some() && !valid(lua, goal.target) {
        return Progress::Failed;
    }
    let Some(to) = goal_point(lua, &goal, &hull) else { return Progress::Failed };
    if arrived(start, to, goal.tolerance, &hull) {
        stop(lua, handle);
        return Progress::Arrived;
    }
    let now = now(lua);
    let mut route = route;
    // the way to what moves, found again when it has gone from the way's end
    let moved_off = route.last().is_none_or(|end| (*end - to).length() > 32.0);
    if goal.target.index.is_some() && moved_off && now >= next_route {
        if let Some(found) = nav::route(lua, start, to, &hull) {
            route = found;
        }
        npc::with_npc(lua, handle, |_, npc| npc.mind.next_route = now + REROUTE);
    }
    if route.is_empty() {
        route.push(to);
    }
    let move_act = npc::with_npc(lua, handle, |_, npc| npc.movement_activity).unwrap_or(ACT_WALK);
    ensure_activity(lua, handle, &[move_act, ACT_WALK, ACT_RUN]);
    let mut budget = speed(lua, handle) * dt as f32;
    let solid = Solid::AllBut(vec![handle]);
    let mut at = start;
    let mut blocker = EntityHandle::NULL;
    while budget > 0.01 && !route.is_empty() {
        let left = if hull.air { route[0] - at } else { flat(route[0] - at) };
        let length = left.length();
        if length < 1.0 {
            route.remove(0);
            // a point past it that is in reach goes first
            if route.len() >= 2 && nav::reaches(lua, at, route[1], &hull, &Solid::World) {
                route.remove(0);
            }
            continue;
        }
        let step = budget.min(length);
        match go(lua, at, left * (step / length), &hull, &solid) {
            Ok(next) => {
                budget -= step;
                at = next;
            }
            Err((_, hit)) => {
                blocker = hit;
                break;
            }
        }
    }
    entity::set_world_pos(lua, handle, at);
    let moved = at - start;
    let heading = route.first().copied().unwrap_or(to);
    npc::turn_towards(lua, handle, heading, (dt * 2.0) as f32);
    let expected = speed(lua, handle) * dt as f32;
    let stalled = npc::with_npc(lua, handle, |ent, npc| {
        let mind = &mut npc.mind;
        mind.route = route.clone();
        mind.velocity = moved * (1.0 / dt as f32);
        ent.velocity = mind.velocity;
        mind.blocker = blocker;
        if moved.length() < expected * 0.25 {
            mind.stalled += dt;
        } else {
            mind.stalled = 0.0;
        }
        mind.stalled
    })
    .unwrap_or(0.0);
    if arrived(at, to, goal.tolerance, &hull) || (route.is_empty() && goal.target.index.is_none()) {
        stop(lua, handle);
        return Progress::Arrived;
    }
    if stalled > STALL_TIME {
        stop(lua, handle);
        return Progress::Failed;
    }
    Progress::Moving
}

// --- fighting ---

/// How an NPC fights.
#[derive(Clone, Copy)]
enum Attack {
    Gun(EntityHandle, &'static NpcGun),
    Swep(EntityHandle),
    Melee(&'static Melee),
    Nothing,
}

fn attack_of(lua: &Lua, handle: EntityHandle) -> Attack {
    if let Some(weapon) = npc::weapon_of(lua, handle) {
        let Some((class, scripted)) = host(lua).entities.get(weapon).map(|e| (e.class.to_ascii_lowercase(), e.scripted)) else {
            return Attack::Nothing;
        };
        if scripted {
            return Attack::Swep(weapon);
        }
        if let Some(gun) = NPC_GUNS.iter().find(|g| g.class == class) {
            return Attack::Gun(weapon, gun);
        }
        if let Some(melee) = MELEE.iter().find(|m| m.class == class) {
            return Attack::Melee(melee);
        }
        return Attack::Nothing;
    }
    let class = host(lua).entities.get(handle).map(|e| e.class.to_ascii_lowercase()).unwrap_or_default();
    MELEE.iter().find(|m| m.class == class).map_or(Attack::Nothing, Attack::Melee)
}

/// Whether an NPC sees a target: nothing opaque between its eyes and the target's middle or top.
pub(crate) fn sees(lua: &Lua, handle: EntityHandle, target: EntityHandle) -> bool {
    let Some(from) = npc::eyes(lua, handle) else { return false };
    let top = crate::player::eye(lua, target).map(|(e, _)| e).or_else(|| npc::eyes(lua, target));
    [middle(lua, target), top].into_iter().flatten().any(|to| {
        let sweep = crate::trace::Sweep {
            mask: gmod_bsp::mask::OPAQUE,
            ..crate::trace::Sweep::line(from, to, crate::trace::Filter::Entities(vec![handle]))
        };
        let hit = crate::trace::trace(lua, &sweep);
        hit.fraction >= 1.0 || hit.entity == target
    })
}

/// How far a gun of an NPC reaches: a SWEP as far as the engine's pistol.
fn gun_range(attack: Attack) -> f32 {
    match attack {
        Attack::Gun(_, gun) => gun.range,
        Attack::Swep(_) => NPC_GUNS[0].range,
        _ => 0.0,
    }
}

/// Whether an NPC may shoot its enemy now: in range, seen, no friend in the way.
fn can_shoot(lua: &Lua, handle: EntityHandle, enemy: EntityHandle, attack: Attack) -> bool {
    let (Some(from), Some(to)) = (npc::eyes(lua, handle), middle(lua, enemy)) else { return false };
    if (to - from).length() > gun_range(attack) || !sees(lua, handle, enemy) {
        return false;
    }
    let sweep = crate::trace::Sweep {
        mask: gmod_bsp::collision::mask::SHOT_HULL,
        ..crate::trace::Sweep::line(from, to, crate::trace::Filter::Entities(vec![handle]))
    };
    let hit = crate::trace::trace(lua, &sweep);
    let friend = hit.entity.index.is_some_and(|i| i != 0) && hit.entity != enemy && npc::disposition(lua, handle, hit.entity).0 == D_LI;
    !friend
}

/// How far into a strike's reach a target is: the distance of the middles, less the target's
/// half width; whether that is within `range`.
fn in_reach(lua: &Lua, handle: EntityHandle, target: EntityHandle, range: f32) -> bool {
    let (Some(a), Some(b)) = (middle(lua, handle), middle(lua, target)) else { return false };
    let half = host(lua).entities.get(target).map_or(0.0, |e| (e.maxs.x - e.mins.x).max(e.maxs.y - e.mins.y) * 0.5);
    (b - a).length() - half <= range
}

fn melee_damage(lua: &Lua, melee: &Melee) -> f32 {
    if melee.class.starts_with("weapon_") {
        let d = crate::weapon::skill_damage(lua, melee.class, false);
        if d > 0.0 {
            return d;
        }
    }
    if !melee.skill.is_empty()
        && let Some(d) = crate::weapon::skill(lua, melee.skill)
    {
        return d;
    }
    melee.damage
}

/// A strike lands on what it was swung at, if that is still in reach and before the NPC.
fn strike(lua: &Lua, handle: EntityHandle, target: EntityHandle, melee: &Melee) {
    if !npc::living(lua, target) || !in_reach(lua, handle, target, melee.range * 1.3) {
        return;
    }
    let Some(to) = middle(lua, target) else { return };
    if !facing(lua, handle, to, SWING_CONE) {
        return;
    }
    let from = middle(lua, handle).unwrap_or(to);
    let damage = melee_damage(lua, melee);
    let mut info = DamageInfo::new(damage, melee.kind, handle, handle);
    info.position = to;
    info.force = (to - from).normalized() * (damage * 75.0);
    damage::take(lua, target, info);
}

/// The weapon's clip: rounds in it and its size (-1 for none).
fn clip(lua: &Lua, weapon: EntityHandle) -> (i64, i64) {
    let size = crate::weapon::data(lua, weapon).map_or(-1, |d| d.clip_size[0]);
    let rounds = host(lua).entities.get(weapon).and_then(|e| e.weapon.as_ref().map(|w| w.clip[0])).unwrap_or(-1);
    (rounds, size)
}

/// Whether a gun of an NPC is out of rounds.
fn needs_reload(lua: &Lua, attack: Attack) -> bool {
    match attack {
        Attack::Gun(weapon, _) | Attack::Swep(weapon) => {
            let (rounds, size) = clip(lua, weapon);
            size > 0 && rounds <= 0
        }
        _ => false,
    }
}

/// An engine gun in an NPC's hands fires once at its enemy.
fn fire_gun(lua: &Lua, handle: EntityHandle, enemy: EntityHandle, weapon: EntityHandle, gun: &NpcGun) {
    let data = crate::weapon::data(lua, weapon).unwrap_or_default();
    let (Some(from), Some(to)) = (npc::eyes(lua, handle), middle(lua, enemy)) else { return };
    let proficiency = npc::with_npc(lua, handle, |_, npc| npc.proficiency).unwrap_or(1).clamp(0, 4) as usize;
    let cone = gun.cone + PROFICIENCY_CONE[proficiency];
    let spread = (cone.to_radians() * 0.5).sin();
    let mut b = Bullets::new(handle, from, (to - from).normalized());
    b.spread = Vector::new(spread, spread, 0.0);
    b.num = gun.pellets;
    b.ammo_type = data.ammo[0].clone();
    b.tracer = 1;
    bullets::fire(lua, handle, b);
    if let Some(sound) = data.sound("single_shot_npc").or_else(|| data.sound("single_shot")) {
        crate::sound::emit_scaled(lua, sound, weapon, 1.0);
    }
    let now = now(lua);
    host(lua).entities.with_mut(weapon, |ent| {
        if let Some(w) = ent.weapon.as_mut() {
            if data.clip_size[0] > 0 {
                w.clip[0] = (w.clip[0] - 1).max(0);
            }
            w.last_shoot = now;
        }
    });
}

/// A SWEP in an NPC's hands fires once: `NPCShoot_Primary` with where from and which way, else
/// `PrimaryAttack`.
fn fire_swep(lua: &Lua, handle: EntityHandle, enemy: EntityHandle, weapon: EntityHandle) {
    let (Some(from), Some(to)) = (npc::eyes(lua, handle), middle(lua, enemy)) else { return };
    let (Ok(src), Ok(dir)) = (entity::vector(lua, from), entity::vector(lua, (to - from).normalized())) else { return };
    if crate::weapon::call(lua, weapon, "NPCShoot_Primary", MultiValue::from_iter([src, dir])).is_none() {
        crate::weapon::call(lua, weapon, "PrimaryAttack", MultiValue::new());
    }
}

/// Two numbers a SWEP's function answers, or these.
fn swep_pair(lua: &Lua, weapon: EntityHandle, name: &str, default: (f64, f64)) -> (f64, f64) {
    let answer = crate::weapon::call(lua, weapon, name, MultiValue::new()).unwrap_or_default();
    let n = |i: usize, d: f64| match answer.get(i) {
        Some(Value::Integer(n)) => *n as f64,
        Some(Value::Number(n)) => *n,
        _ => d,
    };
    (n(0, default.0), n(1, default.1))
}

/// How an NPC's gun bursts: shots at least and at most, the time between, the rest after.
fn bursts(lua: &Lua, attack: Attack) -> ((u32, u32), f64, (f64, f64)) {
    match attack {
        Attack::Gun(_, gun) => (gun.burst, gun.interval, gun.rest),
        Attack::Swep(weapon) => {
            let answer = crate::weapon::call(lua, weapon, "GetNPCBurstSettings", MultiValue::new()).unwrap_or_default();
            let n = |i: usize, d: f64| match answer.get(i) {
                Some(Value::Integer(n)) => *n as f64,
                Some(Value::Number(n)) => *n,
                _ => d,
            };
            let (low, high, interval) = (n(0, 1.0), n(1, 3.0), n(2, 0.3));
            let rest = swep_pair(lua, weapon, "GetNPCRestTimes", (0.3, 0.6));
            ((low.max(1.0) as u32, high.max(low).max(1.0) as u32), interval.max(0.0), rest)
        }
        _ => ((1, 1), 1.0, (1.0, 1.0)),
    }
}

/// The activities of an NPC firing its gun, the gun's own first.
fn firing_acts(lua: &Lua, attack: Attack) -> Vec<i64> {
    let own = match attack {
        Attack::Gun(_, gun) => act_named(lua, gun.act),
        _ => -1,
    };
    vec![own, ACT_RANGE_ATTACK1, ACT_IDLE_ANGRY, ACT_IDLE]
}

// --- the enemy ---

/// The nearest living player or NPC an NPC hates and sees within its look distance.
pub(crate) fn nearest_enemy(lua: &Lua, handle: EntityHandle) -> Option<EntityHandle> {
    let from = npc::eyes(lua, handle)?;
    let range = npc::with_npc(lua, handle, |_, npc| npc.look_distance)?;
    let candidates: Vec<EntityHandle> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter(|&h| {
                h != handle && (matches!(h.kind, Kind::Npc | Kind::NextBot) || entities.get(h).is_some_and(|e| e.player.is_some()))
            })
            .collect()
    };
    let mut best: Option<(EntityHandle, f32)> = None;
    for target in candidates {
        if !npc::living(lua, target) || !npc::targetable(lua, target) || npc::disposition(lua, handle, target).0 != D_HT {
            continue;
        }
        let Some(to) = middle(lua, target) else { continue };
        let distance = (to - from).length();
        if distance > range || best.is_some_and(|(_, d)| d <= distance) {
            continue;
        }
        if sees(lua, handle, target) {
            best = Some((target, distance));
        }
    }
    best.map(|(target, _)| target)
}

/// The NPC's enemy, while it is alive.
fn enemy_of(lua: &Lua, handle: EntityHandle) -> Option<EntityHandle> {
    npc::with_npc(lua, handle, |_, npc| npc.enemy).filter(|&e| npc::living(lua, e))
}

fn unreachable(lua: &Lua, handle: EntityHandle, target: EntityHandle) -> bool {
    let now = now(lua);
    npc::with_npc(lua, handle, |_, npc| npc.mind.unreachable.iter().any(|&(t, until)| t == target && until > now)).unwrap_or(false)
}

pub(crate) fn remember_unreachable(lua: &Lua, handle: EntityHandle, target: EntityHandle, seconds: f64) {
    let until = now(lua) + seconds;
    npc::with_npc(lua, handle, |_, npc| {
        npc.mind.unreachable.retain(|&(t, _)| t != target);
        npc.mind.unreachable.push((target, until));
    });
}

pub(crate) fn is_unreachable(lua: &Lua, handle: EntityHandle, target: EntityHandle) -> bool {
    unreachable(lua, handle, target)
}

// --- schedules ---

/// What running a schedule for a tick came to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    Running,
    Done,
}

/// The schedule an NPC chooses when the last is done.
fn select(lua: &Lua, handle: EntityHandle) -> i64 {
    let Some(enemy) = enemy_of(lua, handle) else { return sched::IDLE_STAND };
    let attack = attack_of(lua, handle);
    let moves = hull_of(lua, handle).is_some() && !unreachable(lua, handle, enemy);
    match attack {
        Attack::Gun(..) | Attack::Swep(_) if needs_reload(lua, attack) => sched::RELOAD,
        Attack::Gun(..) | Attack::Swep(_) if can_shoot(lua, handle, enemy, attack) => sched::RANGE_ATTACK1,
        Attack::Melee(m) if in_reach(lua, handle, enemy, m.range) => sched::MELEE_ATTACK1,
        Attack::Gun(..) | Attack::Swep(_) | Attack::Melee(_) if moves => sched::CHASE_ENEMY,
        _ => sched::COMBAT_FACE,
    }
}

/// Lua set a schedule: it starts anew, even the one the NPC runs.
pub(crate) fn schedule_set(npc: &mut npc::Npc) {
    npc.mind.started = None;
}

fn set_schedule(lua: &Lua, handle: EntityHandle, schedule: i64) {
    npc::with_npc(lua, handle, |_, npc| {
        npc.schedule = schedule;
        npc.mind.started = None;
    });
}

/// A schedule starts: the goal it goes to. Whether it could.
fn start(lua: &Lua, handle: EntityHandle, schedule: i64) -> bool {
    let (enemy, target, last) = npc::with_npc(lua, handle, |_, npc| (npc.enemy, npc.target, npc.last_position)).unwrap_or_default();
    match schedule {
        sched::FORCED_GO | sched::FORCED_GO_RUN => {
            set_goal(lua, handle, Goal::point(last, POINT_TOLERANCE, schedule == sched::FORCED_GO_RUN))
        }
        sched::IDLE_WANDER | sched::IDLE_WALK | sched::PATROL_WALK | sched::PATROL_RUN | sched::RUN_RANDOM => {
            let run = matches!(schedule, sched::PATROL_RUN | sched::RUN_RANDOM);
            wander(lua, handle, WANDER.0, WANDER.1, run)
        }
        sched::CHASE_ENEMY => {
            let tolerance = match attack_of(lua, handle) {
                Attack::Melee(m) => m.range * 0.8,
                _ => GUN_CHASE_TOLERANCE,
            };
            let going = valid(lua, enemy) && set_goal(lua, handle, Goal::after(enemy, Vector::default(), tolerance, true));
            // with no way to it, it is not tried again for a while
            if !going && valid(lua, enemy) {
                remember_unreachable(lua, handle, enemy, UNREACHABLE_TIME);
            }
            going
        }
        sched::TARGET_CHASE => valid(lua, target) && set_goal(lua, handle, Goal::after(target, Vector::default(), TARGET_TOLERANCE, true)),
        sched::RUN_FROM_ENEMY | sched::TAKE_COVER_FROM_ENEMY | sched::MOVE_AWAY_FROM_ENEMY => flee(lua, handle, enemy),
        sched::RELOAD => {
            let took = crate::anim::rig(lua, &host(lua).entities.get(handle).map(|e| e.model.clone()).unwrap_or_default())
                .is_some_and(|r| crate::anim::has_activity(lua, &r, ACT_RELOAD));
            stop(lua, handle);
            if took {
                npc::play_activity(lua, handle, ACT_RELOAD);
            }
            let until = now(lua) + RELOAD_TIME;
            npc::with_npc(lua, handle, |_, npc| npc.mind.reload_until = Some(until));
            true
        }
        sched::RANGE_ATTACK1 | sched::MELEE_ATTACK1 | sched::COMBAT_FACE | sched::TARGET_FACE | sched::NPC_FREEZE => {
            stop(lua, handle);
            true
        }
        // standing keeps a goal Lua gave
        _ => true,
    }
}

/// Sets out for a spot from `min` to `max` away.
fn wander(lua: &Lua, handle: EntityHandle, min: f32, max: f32, run: bool) -> bool {
    let (Some(hull), Some(from)) = (hull_of(lua, handle), pos(lua, handle)) else { return false };
    let roll = |below: usize| crate::anim::random(lua, below as i64) as usize;
    let Some(spot) = nav::random_spot(lua, from, min, max, &hull, roll) else { return false };
    set_goal(lua, handle, Goal::point(spot, POINT_TOLERANCE, run))
}

/// Sets out for the spot of a few that is farthest from the enemy.
fn flee(lua: &Lua, handle: EntityHandle, enemy: EntityHandle) -> bool {
    let (Some(hull), Some(from), Some(threat)) = (hull_of(lua, handle), pos(lua, handle), pos(lua, enemy)) else { return false };
    let roll = |below: usize| crate::anim::random(lua, below as i64) as usize;
    let best = (0..6)
        .filter_map(|_| nav::random_spot(lua, from, FLEE.0, FLEE.1, &hull, roll))
        .max_by(|a, b| (*a - threat).length().total_cmp(&(*b - threat).length()));
    best.is_some_and(|spot| {
        (spot - threat).length() > (from - threat).length() && set_goal(lua, handle, Goal::point(spot, POINT_TOLERANCE, true))
    })
}

/// A tick of a schedule.
fn run(lua: &Lua, handle: EntityHandle, schedule: i64, dt: f64) -> Outcome {
    let now = now(lua);
    let enemy = enemy_of(lua, handle);
    let attack = attack_of(lua, handle);
    let moving = |lua: &Lua| match move_on(lua, handle, dt) {
        Progress::Moving => Outcome::Running,
        _ => Outcome::Done,
    };
    match schedule {
        sched::NPC_FREEZE => Outcome::Running,
        sched::FORCED_GO | sched::FORCED_GO_RUN | sched::TARGET_CHASE => moving(lua),
        sched::RUN_FROM_ENEMY | sched::TAKE_COVER_FROM_ENEMY | sched::MOVE_AWAY_FROM_ENEMY => moving(lua),
        sched::IDLE_WANDER | sched::IDLE_WALK | sched::PATROL_WALK | sched::PATROL_RUN | sched::RUN_RANDOM => {
            if enemy.is_some() {
                stop(lua, handle);
                return Outcome::Done;
            }
            let patrol = matches!(schedule, sched::PATROL_WALK | sched::PATROL_RUN);
            let (going, rest_until) =
                npc::with_npc(lua, handle, |_, npc| (npc.mind.goal.is_some(), npc.mind.rest_until)).unwrap_or_default();
            if going {
                if move_on(lua, handle, dt) == Progress::Moving {
                    return Outcome::Running;
                }
                if !patrol {
                    return Outcome::Done;
                }
                // a patrol rests at each end, then goes on
                let rest = roll(lua, PATROL_REST.0, PATROL_REST.1);
                npc::with_npc(lua, handle, |_, npc| npc.mind.rest_until = now + rest);
                return Outcome::Running;
            }
            if !patrol {
                return Outcome::Done;
            }
            if now >= rest_until && !wander(lua, handle, WANDER.0, WANDER.1, schedule == sched::PATROL_RUN) {
                npc::with_npc(lua, handle, |_, npc| npc.mind.rest_until = now + PATROL_REST.0);
            }
            Outcome::Running
        }
        sched::CHASE_ENEMY => {
            let Some(enemy) = enemy else {
                stop(lua, handle);
                return Outcome::Done;
            };
            let ready = match attack {
                Attack::Melee(m) => in_reach(lua, handle, enemy, m.range),
                Attack::Gun(..) | Attack::Swep(_) => can_shoot(lua, handle, enemy, attack),
                Attack::Nothing => true,
            };
            if ready {
                stop(lua, handle);
                return Outcome::Done;
            }
            match move_on(lua, handle, dt) {
                Progress::Moving => Outcome::Running,
                Progress::Arrived => Outcome::Done,
                Progress::Failed => {
                    remember_unreachable(lua, handle, enemy, UNREACHABLE_TIME);
                    Outcome::Done
                }
            }
        }
        sched::TARGET_FACE => {
            let target = npc::with_npc(lua, handle, |_, npc| npc.target).unwrap_or(EntityHandle::NULL);
            match pos(lua, target) {
                Some(at) if !face(lua, handle, at, 5.0) => Outcome::Running,
                _ => Outcome::Done,
            }
        }
        sched::COMBAT_FACE => {
            let Some(enemy) = enemy else { return Outcome::Done };
            if let Some(at) = pos(lua, enemy) {
                face(lua, handle, at, 5.0);
            }
            // it fights as soon as it may
            if select(lua, handle) != sched::COMBAT_FACE { Outcome::Done } else { Outcome::Running }
        }
        sched::RELOAD => {
            let until = npc::with_npc(lua, handle, |_, npc| npc.mind.reload_until).flatten().unwrap_or(0.0);
            if now < until {
                return Outcome::Running;
            }
            if let Attack::Gun(weapon, _) | Attack::Swep(weapon) = attack {
                let (_, size) = clip(lua, weapon);
                host(lua).entities.with_mut(weapon, |ent| {
                    if let Some(w) = ent.weapon.as_mut() {
                        w.clip[0] = size;
                    }
                });
            }
            npc::with_npc(lua, handle, |_, npc| npc.mind.reload_until = None);
            ensure_activity(lua, handle, &[ACT_IDLE_ANGRY, ACT_IDLE]);
            Outcome::Done
        }
        sched::RANGE_ATTACK1 => {
            let Some(enemy) = enemy else { return Outcome::Done };
            if needs_reload(lua, attack) || !can_shoot(lua, handle, enemy, attack) {
                npc::with_npc(lua, handle, |_, npc| npc.mind.burst = 0);
                return Outcome::Done;
            }
            let Some(at) = middle(lua, enemy) else { return Outcome::Done };
            let aimed = face(lua, handle, at, AIM_CONE);
            let (next, burst) = npc::with_npc(lua, handle, |_, npc| (npc.mind.next_attack, npc.mind.burst)).unwrap_or_default();
            if !aimed || now < next {
                return Outcome::Running;
            }
            let ((low, high), interval, rest) = bursts(lua, attack);
            let burst = if burst == 0 {
                ensure_activity(lua, handle, &firing_acts(lua, attack));
                low + crate::anim::random(lua, i64::from(high - low + 1)) as u32
            } else {
                burst
            };
            match attack {
                Attack::Gun(weapon, gun) => fire_gun(lua, handle, enemy, weapon, gun),
                Attack::Swep(weapon) => fire_swep(lua, handle, enemy, weapon),
                _ => {}
            }
            let left = burst.saturating_sub(1);
            let wait = if left > 0 { interval } else { roll(lua, rest.0, rest.1) };
            npc::with_npc(lua, handle, |_, npc| {
                npc.mind.burst = left;
                npc.mind.next_attack = now + wait;
            });
            Outcome::Running
        }
        sched::MELEE_ATTACK1 => {
            let Attack::Melee(melee) = attack else { return Outcome::Done };
            let (swing, next) = npc::with_npc(lua, handle, |_, npc| (npc.mind.swing, npc.mind.next_attack)).unwrap_or_default();
            if let Some((lands, target)) = swing {
                if now >= lands {
                    npc::with_npc(lua, handle, |_, npc| npc.mind.swing = None);
                    strike(lua, handle, target, melee);
                }
                return Outcome::Running;
            }
            let Some(enemy) = enemy else { return Outcome::Done };
            if !in_reach(lua, handle, enemy, melee.range) {
                return Outcome::Done;
            }
            let Some(at) = middle(lua, enemy) else { return Outcome::Done };
            if !face(lua, handle, at, SWING_CONE * 0.75) || now < next {
                return Outcome::Running;
            }
            npc::play_activity(lua, handle, ACT_MELEE_ATTACK1);
            npc::with_npc(lua, handle, |_, npc| {
                npc.mind.swing = Some((now + melee.delay, enemy));
                npc.mind.next_attack = now + melee.interval;
            });
            if melee.delay <= 0.0 {
                npc::with_npc(lua, handle, |_, npc| npc.mind.swing = None);
                strike(lua, handle, enemy, melee);
            }
            Outcome::Running
        }
        // standing: a goal Lua gave is walked to, an enemy makes it choose again
        _ => {
            let going = npc::with_npc(lua, handle, |_, npc| npc.mind.goal.is_some()).unwrap_or(false);
            if going {
                move_on(lua, handle, dt);
            }
            if enemy.is_some() && !going {
                return Outcome::Done;
            }
            Outcome::Running
        }
    }
}

/// A tick of an NPC's schedule ([`crate::npc::tick`]), after it looked for its enemy.
pub(crate) fn think(lua: &Lua, handle: EntityHandle, dt: f64) {
    let Some((schedule, started)) = npc::with_npc(lua, handle, |_, npc| (npc.schedule, npc.mind.started)) else { return };
    if started != Some(schedule) {
        npc::with_npc(lua, handle, |_, npc| npc.mind.started = Some(schedule));
        if !start(lua, handle, schedule) {
            set_schedule(lua, handle, select(lua, handle));
            return;
        }
    }
    if run(lua, handle, schedule, dt) == Outcome::Done {
        set_schedule(lua, handle, select(lua, handle));
    }
    // an NPC that is not going anywhere faces its enemy
    let (going, schedule) = npc::with_npc(lua, handle, |_, npc| (npc.mind.goal.is_some(), npc.schedule)).unwrap_or((true, 0));
    if !going
        && schedule != sched::NPC_FREEZE
        && let Some(at) = enemy_of(lua, handle).and_then(|e| pos(lua, e))
    {
        npc::turn_towards(lua, handle, at, crate::timer::TICK_INTERVAL as f32);
    }
    // what it plays when it stands: done with an attack, it idles
    let idle = npc::with_npc(lua, handle, |_, npc| npc.mind.goal.is_none() && npc.mind.swing.is_none() && npc.mind.reload_until.is_none())
        .unwrap_or(false);
    if idle && host(lua).entities.get(handle).is_some_and(|e| e.anim.finished || matches!(e.anim.activity, ACT_WALK | ACT_RUN)) {
        let angry = enemy_of(lua, handle).is_some();
        ensure_activity(lua, handle, if angry { &[ACT_IDLE_ANGRY, ACT_IDLE] } else { &[ACT_IDLE] });
    }
}
