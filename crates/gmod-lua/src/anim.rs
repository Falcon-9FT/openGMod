//! What entities play on the bones of their models, as the engine keeps it.
//!
//! An entity with a studio model plays a sequence of it (`SetSequence`, `ResetSequence`) at a
//! point of its cycle (`SetCycle`, 0 to 1) at a rate (`SetPlaybackRate`), with its pose
//! parameters (`SetPoseParameter`) and layers over it: gestures and layered sequences, the
//! gesture slots of players. The engine moves the cycle on every tick for what animates by
//! itself - players, view models, NPCs, `prop_dynamic`, and scripted entities that ask
//! (`ENT.AutomaticFrameAdvance`) - at the sequence's own rate
//! ([`gmod_model::Rig::cycle_rate`]); others move when Lua says (`FrameAdvance`). Where the
//! bones are follows (`model_bones`): what `GetBonePosition`, `GetAttachment` and drawing
//! give. An entity merged onto its parent (`EF_BONEMERGE`: the hands on the view model) takes
//! its parent's bones of the same names.
//!
//! A player's view model plays the weapon in hand: a weapon taken out puts its view model on
//! and plays drawing it (`ACT_VM_DRAW`), `SendWeaponAnim` plays the sequence of an activity, and
//! an engine weapon goes back to idle (`ACT_VM_IDLE`) once what it sent has played and nothing
//! is pressed; a reload takes as long as its sequence. A player's own model plays what the
//! gamemode chooses every tick (`CalcMainActivity`, `TranslateActivity`, `UpdateAnimation`),
//! its legs and aim set by the engine's pose parameters.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use gmod_model::pose::{self as rig_pose, Matrix as BoneMatrix};
use gmod_model::{Layer, Rig};
use mlua::{Lua, MultiValue, ObjectLike, Value};

use crate::args::Args;
use crate::entity::{self, Def, DefMany, EntityHandle, Kind};
use crate::matrix::Matrix;
use crate::vector::{Angle, Vector};
use crate::{host, player};

/// Activities models name that the game's enumerations do not have are numbered after this.
const LAST_SHARED_ACTIVITY: i64 = 2045;
/// How many layers an entity plays over its sequence at most, as the engine has.
pub const MAX_LAYERS: usize = 15;
/// `EF_BONEMERGE`.
const EF_BONEMERGE: i64 = 1;
const ACT_VM_DRAW: i64 = 172;
const ACT_VM_IDLE: i64 = 174;
const ACT_MP_STAND_IDLE: i64 = 990;
/// Player animation events (`PLAYERANIMEVENT_*`) the engine raises itself.
pub(crate) const EVENT_ATTACK_PRIMARY: i64 = 0;
pub(crate) const EVENT_ATTACK_SECONDARY: i64 = 1;
pub(crate) const EVENT_RELOAD: i64 = 3;
pub(crate) const EVENT_JUMP: i64 = 6;
pub(crate) const EVENT_CUSTOM: i64 = 19;

/// A sequence played over an entity's own.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimLayer {
    /// Its number, 0 to [`MAX_LAYERS`].
    pub id: usize,
    pub sequence: i64,
    pub cycle: f32,
    pub weight: f32,
    pub playback_rate: f32,
    pub looping: bool,
    /// It goes when it has played: a gesture.
    pub autokill: bool,
    pub finished: bool,
    pub priority: i64,
    /// The activity of a gesture, or -1.
    pub activity: i64,
    /// A player's gesture slot (`GESTURE_SLOT_*`), or -1.
    pub slot: i64,
    /// Parts of its cycle over which it fades in at the start and out at the end.
    pub blend_in: f32,
    pub blend_out: f32,
}

impl AnimLayer {
    fn new(id: usize, sequence: i64) -> AnimLayer {
        AnimLayer {
            id,
            sequence,
            cycle: 0.0,
            weight: 1.0,
            playback_rate: 1.0,
            looping: false,
            autokill: false,
            finished: false,
            priority: 0,
            activity: -1,
            slot: -1,
            blend_in: 0.0,
            blend_out: 0.0,
        }
    }

    /// Its weight at this point of its cycle, faded in and out.
    fn effective_weight(&self) -> f32 {
        let mut weight = self.weight;
        if self.blend_in > 0.0 && self.cycle < self.blend_in {
            weight *= self.cycle / self.blend_in;
        }
        if self.blend_out > 0.0 && self.cycle > 1.0 - self.blend_out {
            weight *= (1.0 - self.cycle) / self.blend_out;
        }
        weight.max(0.0)
    }
}

/// What an entity plays.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimState {
    pub sequence: i64,
    pub cycle: f32,
    pub playback_rate: f32,
    /// When the cycle was last moved on (`GetAnimTime`).
    pub anim_time: f64,
    /// A sequence that does not loop has played to its end.
    pub finished: bool,
    /// The activity a player's own model plays; -1 for none.
    pub activity: i64,
    /// The pose parameters set, from 0 to 1 across their ranges, by number; NaN for one not set.
    pub pose: Vec<f32>,
    pub layers: Vec<AnimLayer>,
    /// Where a player's legs point, in degrees: the aim turns the rest of the body from there.
    pub feet_yaw: f32,
}

impl Default for AnimState {
    fn default() -> AnimState {
        AnimState {
            sequence: 0,
            cycle: 0.0,
            playback_rate: 1.0,
            anim_time: 0.0,
            finished: false,
            activity: -1,
            pose: Vec::new(),
            layers: Vec::new(),
            feet_yaw: 0.0,
        }
    }
}

impl AnimState {
    /// The pose parameters for a rig: those set, the others at their starting values.
    pub fn parameters(&self, rig: &Rig) -> Vec<f32> {
        let mut out = rig.default_pose_parameters();
        for (slot, value) in out.iter_mut().zip(&self.pose) {
            if !value.is_nan() {
                *slot = *value;
            }
        }
        out
    }

    fn set_pose(&mut self, index: usize, normalized: f32) {
        if self.pose.len() <= index {
            self.pose.resize(index + 1, f32::NAN);
        }
        self.pose[index] = normalized;
    }
}

/// Rigs of the models entities have, each read once, and the activities models name.
#[derive(Default)]
pub(crate) struct Anims {
    rigs: RefCell<HashMap<String, Option<Arc<Rig>>>>,
    /// Activities of models that the game's enumerations do not have, in the order they came.
    private: RefCell<Vec<String>>,
    /// For choosing among sequences of an activity.
    random: Cell<u64>,
}

/// The rig of a studio model (with the models it includes and its `.ani`), read once.
pub(crate) fn rig(lua: &Lua, model: &str) -> Option<Arc<Rig>> {
    if model.len() < 5 || !model[model.len() - 4..].eq_ignore_ascii_case(".mdl") {
        return None;
    }
    let host = host(lua);
    let path = model.replace('\\', "/").to_ascii_lowercase();
    if let Some(rig) = host.anims.rigs.borrow().get(&path) {
        return rig.clone();
    }
    let rig = Rig::load(&host.vfs, &path).ok().map(Arc::new);
    host.anims.rigs.borrow_mut().insert(path, rig.clone());
    rig
}

/// The number of an activity name (`ACT_VM_IDLE`): the game's, or one a model names that is
/// numbered after the game's, as the engine numbers them; -1 for none.
pub(crate) fn activity_id(lua: &Lua, name: &str) -> i64 {
    if name.is_empty() {
        return -1;
    }
    if let Some(id) = crate::engine::activity(name) {
        return id;
    }
    let host = host(lua);
    let mut private = host.anims.private.borrow_mut();
    let index = match private.iter().position(|n| n.eq_ignore_ascii_case(name)) {
        Some(i) => i,
        None => {
            private.push(name.to_ascii_uppercase());
            private.len() - 1
        }
    };
    LAST_SHARED_ACTIVITY + 1 + index as i64
}

/// The name of an activity's number.
pub(crate) fn activity_name(lua: &Lua, id: i64) -> Option<String> {
    if let Some(name) = crate::engine::activity_name(id) {
        return Some(name.to_string());
    }
    let index = usize::try_from(id - LAST_SHARED_ACTIVITY - 1).ok()?;
    host(lua).anims.private.borrow().get(index).cloned()
}

/// A number from 0 up to `below`, for choosing.
fn random(lua: &Lua, below: i64) -> i64 {
    let cell = &host(lua).anims.random;
    // xorshift, seeded the first time
    let mut x = cell.get();
    if x == 0 {
        x = 0x9e37_79b9_7f4a_7c15;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    cell.set(x);
    (x % below.max(1) as u64) as i64
}

/// The sequences of an activity, each as likely as its weight says.
fn weighted(lua: &Lua, rig: &Rig, act: i64, pick: impl FnOnce(i64) -> i64) -> i64 {
    let choices: Vec<(usize, i64)> = (0..rig.sequence_count())
        .filter_map(|s| {
            let seq = rig.sequence(s)?;
            (activity_id(lua, &seq.activity) == act).then_some((s, i64::from(seq.activity_weight.max(1))))
        })
        .collect();
    let total: i64 = choices.iter().map(|&(_, w)| w).sum();
    if total == 0 {
        return -1;
    }
    let mut roll = pick(total);
    for (s, weight) in choices {
        if roll < weight {
            return s as i64;
        }
        roll -= weight;
    }
    -1
}

/// `SelectWeightedSequence`: a sequence of an activity, chosen by weight; -1 for none.
pub(crate) fn select_weighted(lua: &Lua, rig: &Rig, act: i64) -> i64 {
    weighted(lua, rig, act, |total| random(lua, total))
}

/// What of an entity's animation goes with its model.
fn animated(lua: &Lua, handle: EntityHandle) -> Option<(Arc<Rig>, AnimState)> {
    let (model, anim) = host(lua).entities.get(handle).map(|ent| (ent.model.clone(), ent.anim.clone()))?;
    Some((rig(lua, &model)?, anim))
}

fn change(lua: &Lua, handle: EntityHandle, f: impl FnOnce(&mut AnimState)) {
    host(lua).entities.with_mut(handle, |ent| f(&mut ent.anim));
}

fn now(lua: &Lua) -> f64 {
    host(lua).clock.cur_time.get()
}

/// A sequence of the rig an entity's number means, if it has it.
fn sequence_of(rig: &Rig, sequence: i64) -> Option<usize> {
    usize::try_from(sequence).ok().filter(|&s| s < rig.sequence_count())
}

/// `ResetSequence`: plays a sequence from its start if the one before did not loop, keeping
/// where a looping one was otherwise; it is not finished.
pub(crate) fn reset_sequence(lua: &Lua, handle: EntityHandle, sequence: i64) {
    let looping = animated(lua, handle).and_then(|(rig, anim)| Some(rig.sequence(sequence_of(&rig, anim.sequence)?)?.looping()));
    let now = now(lua);
    change(lua, handle, |anim| {
        if !looping.unwrap_or(false) {
            anim.cycle = 0.0;
        }
        anim.sequence = sequence;
        anim.finished = false;
        anim.anim_time = now;
    });
}

/// Plays a sequence from its start, at the rate the entity has.
pub(crate) fn start(lua: &Lua, handle: EntityHandle, sequence: i64) {
    let now = now(lua);
    change(lua, handle, |anim| {
        anim.sequence = sequence;
        anim.cycle = 0.0;
        anim.finished = false;
        anim.anim_time = now;
    });
}

/// Plays a sequence from its start at the normal rate: what a view model does with what the
/// weapon sends.
pub(crate) fn play(lua: &Lua, handle: EntityHandle, sequence: i64) {
    start(lua, handle, sequence);
    change(lua, handle, |anim| anim.playback_rate = 1.0);
}

/// Moves a cycle on by `seconds` at `rate` cycles a second: round for what loops, to its end
/// (and finished) for what does not.
fn moved(cycle: f32, seconds: f64, rate: f32, looping: bool) -> (f32, bool) {
    let cycle = cycle + (seconds * f64::from(rate)) as f32;
    if looping {
        (cycle - cycle.floor(), false)
    } else if cycle >= 1.0 {
        (1.0, true)
    } else if cycle <= 0.0 && rate < 0.0 {
        (0.0, true)
    } else {
        (cycle.max(0.0), false)
    }
}

/// `FrameAdvance`: moves an entity's cycle on by `interval` seconds, or by the time since it
/// was last moved; and its layers with it, dropping gestures that have played.
pub(crate) fn frame_advance(lua: &Lua, handle: EntityHandle, interval: Option<f64>) {
    let Some((rig, anim)) = animated(lua, handle) else { return };
    let now = now(lua);
    let interval = interval.filter(|&i| i > 0.0).unwrap_or(if anim.anim_time > 0.0 { now - anim.anim_time } else { 0.0 });
    let parameters = anim.parameters(&rig);
    let rate = |sequence: i64| {
        sequence_of(&rig, sequence).map_or((0.0, false), |s| (rig.cycle_rate(s, &parameters), rig.sequence(s).is_some_and(|q| q.looping())))
    };
    let (own_rate, looping) = rate(anim.sequence);
    let layer_rates: Vec<(f32, bool)> = anim.layers.iter().map(|l| rate(l.sequence)).collect();
    change(lua, handle, |anim| {
        anim.anim_time = now;
        if interval <= 0.0 {
            return;
        }
        if !anim.finished || looping {
            (anim.cycle, anim.finished) = moved(anim.cycle, interval, own_rate * anim.playback_rate, looping);
        }
        for (layer, (rate, looping)) in anim.layers.iter_mut().zip(layer_rates) {
            if layer.finished && !(layer.looping || looping) {
                continue;
            }
            (layer.cycle, layer.finished) = moved(layer.cycle, interval, rate * layer.playback_rate, layer.looping || looping);
        }
        anim.layers.retain(|l| !(l.autokill && l.finished));
    });
}

/// Whether the engine moves an entity's cycle on by itself every tick.
fn animates_itself(lua: &Lua, handle: EntityHandle) -> bool {
    let Some((kind, class, scripted)) =
        host(lua).entities.get(handle).filter(|e| !e.removed).map(|e| (e.kind, e.class.clone(), e.scripted))
    else {
        return false;
    };
    if matches!(kind, Kind::Npc | Kind::NextBot) {
        return true;
    }
    let class = class.to_ascii_lowercase();
    if class.contains("viewmodel") || class.starts_with("prop_dynamic") || class == "cycler" || class == "prop_door_rotating" {
        return true;
    }
    if scripted && kind == Kind::Entity {
        // a scripted entity that asks for it (`ENT.AutomaticFrameAdvance`, `SetAutomaticFrameAdvance`)
        let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { return false };
        return matches!(ud.get::<Value>("AutomaticFrameAdvance"), Ok(Value::Boolean(true)));
    }
    false
}

/// One tick of the entities that animate by themselves (players animate through their own).
/// A `prop_dynamic` whose animation has played goes back to its default one.
pub(crate) fn advance(lua: &Lua) {
    for handle in host(lua).entities.handles() {
        if animates_itself(lua, handle) {
            frame_advance(lua, handle, None);
            let finished = host(lua).entities.get(handle).is_some_and(|e| e.anim.finished && e.class.starts_with("prop_dynamic"));
            if finished {
                play_default(lua, handle);
            }
        }
    }
}

/// The sequence a `prop_dynamic` plays when nothing else is asked of it (`DefaultAnim`).
fn play_default(lua: &Lua, handle: EntityHandle) {
    let Some(name) = host(lua).entities.get(handle).and_then(|e| e.keyvalue("DefaultAnim").map(str::to_string)) else { return };
    let Some((rig, anim)) = animated(lua, handle) else { return };
    if let Some(sequence) = rig.find_sequence(&name).map(|s| s as i64).filter(|&s| s != anim.sequence || anim.finished) {
        start(lua, handle, sequence);
    }
}

/// An engine entity has spawned: a `prop_dynamic` plays its default animation.
pub(crate) fn spawned(lua: &Lua, handle: EntityHandle) {
    let dynamic = host(lua).entities.get(handle).is_some_and(|e| e.class.to_ascii_lowercase().starts_with("prop_dynamic"));
    if dynamic {
        play_default(lua, handle);
    }
}

/// The inputs of what animates (`prop_dynamic`): `SetAnimation` plays a sequence once (then
/// the default comes back), `SetDefaultAnimation` changes the default, `SetPlaybackRate` the
/// rate. Whether `input` was one of them.
pub(crate) fn input(lua: &Lua, target: EntityHandle, input: &str, param: &str) -> bool {
    match input.to_ascii_lowercase().as_str() {
        "setanimation" => {
            if let Some(sequence) = animated(lua, target).and_then(|(rig, _)| rig.find_sequence(param.trim())) {
                start(lua, target, sequence as i64);
            }
        }
        "setdefaultanimation" => {
            host(lua).entities.with_mut(target, |ent| {
                ent.keyvalues.retain(|(k, _)| !k.eq_ignore_ascii_case("DefaultAnim"));
                ent.keyvalues.push(("DefaultAnim".into(), param.trim().to_string()));
            });
            let finished = host(lua).entities.get(target).is_some_and(|e| e.anim.finished);
            if finished {
                play_default(lua, target);
            }
        }
        "setplaybackrate" => {
            let rate = crate::convar::atof(param) as f32;
            change(lua, target, |anim| anim.playback_rate = rate);
        }
        _ => return false,
    }
    true
}

/// The entity whose place an entity's bones are counted from: its own, or for one merged onto
/// its parent's bones the parent's (whose bones it takes, wherever it stands itself).
fn frame_owner(lua: &Lua, handle: EntityHandle) -> EntityHandle {
    let mut handle = handle;
    for _ in 0..4 {
        let Some((effects, parent)) = host(lua).entities.get(handle).map(|e| (e.effects, e.parent)) else { break };
        if effects & EF_BONEMERGE == 0 || animated(lua, parent).is_none() {
            break;
        }
        handle = parent;
    }
    handle
}

/// Where an entity's model is drawn and its bones are counted from: its origin, its angles
/// and its scale; a merged one's are its parent's.
pub(crate) fn drawn_at(lua: &Lua, handle: EntityHandle) -> Option<(Vector, Angle, f32)> {
    let owner = frame_owner(lua, handle);
    let (pos, angles) = host(lua).entities.transform(owner)?;
    let scale = host(lua).entities.get(owner).map_or(1.0, |e| e.model_scale);
    Some((pos, angles, scale))
}

/// [`drawn_at`] as a matrix.
fn placement(lua: &Lua, handle: EntityHandle) -> Option<BoneMatrix> {
    let (pos, angles, scale) = drawn_at(lua, handle)?;
    let m = Matrix::from_angle(angles).m;
    Some(std::array::from_fn(|r| [m[r][0] * scale, m[r][1] * scale, m[r][2] * scale, [pos.x, pos.y, pos.z][r]]))
}

/// Where the bones of an entity's model are now, bone to model: its sequence at its cycle,
/// its layers, the bones Lua moved (`ManipulateBone*`), and for one merged onto its parent the
/// parent's bones of the same names. None without a studio model.
pub(crate) fn model_bones(lua: &Lua, handle: EntityHandle) -> Option<(Arc<Rig>, Vec<BoneMatrix>)> {
    model_bones_at(lua, handle, 0)
}

fn model_bones_at(lua: &Lua, handle: EntityHandle, depth: u32) -> Option<(Arc<Rig>, Vec<BoneMatrix>)> {
    let (rig, anim) = animated(lua, handle)?;
    let (manipulations, effects, parent) = host(lua).entities.get(handle).map(|e| (e.bone_manipulations.clone(), e.effects, e.parent))?;
    let parameters = anim.parameters(&rig);
    let layers: Vec<Layer> = anim
        .layers
        .iter()
        .filter_map(|l| Some(Layer { sequence: sequence_of(&rig, l.sequence)?, cycle: l.cycle, weight: l.effective_weight() }))
        .collect();
    let mut pose = match sequence_of(&rig, anim.sequence) {
        Some(s) => rig.pose(s, anim.cycle, &parameters, &layers),
        None => rig.rest(),
    };
    for m in &manipulations {
        if let Some(local) = usize::try_from(m.bone).ok().and_then(|b| pose.get_mut(b)) {
            local.position = std::array::from_fn(|i| local.position[i] + [m.position.x, m.position.y, m.position.z][i]);
            let turn = rig_pose::matrix_quaternion(&Matrix::from_angle(m.angles).m[..3].try_into().unwrap());
            local.rotation = rig_pose::quaternion_multiply(local.rotation, turn);
        }
    }
    let mut bones = rig.bone_to_model(&pose);
    for m in manipulations.iter().filter(|m| m.scale != Vector::new(1.0, 1.0, 1.0)) {
        if let Some(bone) = usize::try_from(m.bone).ok().and_then(|b| bones.get_mut(b)) {
            for row in bone.iter_mut() {
                row[0] *= m.scale.x;
                row[1] *= m.scale.y;
                row[2] *= m.scale.z;
            }
        }
    }
    if effects & EF_BONEMERGE != 0
        && depth < 4
        && let Some((parent_rig, parent_bones)) = model_bones_at(lua, parent, depth + 1)
    {
        // the parent's bones of the same names, in the parent's space, which is where a merged
        // entity is drawn ([`drawn_at`])
        let mut merged = vec![false; bones.len()];
        for (b, bone) in rig.bones().iter().enumerate() {
            if let Some(p) = parent_rig.bones().iter().position(|q| q.name.eq_ignore_ascii_case(&bone.name)) {
                bones[b] = parent_bones[p];
                merged[b] = true;
            }
        }
        // a bone the parent does not have stays where it is on its own parent
        for (b, bone) in rig.bones().iter().enumerate() {
            if let Some(p) = usize::try_from(bone.parent).ok().filter(|&p| p < b && !merged[b]) {
                bones[b] = rig_pose::multiply(&bones[p], &rig_pose::local_matrix(&pose[b]));
            }
        }
    }
    Some((rig, bones))
}

/// Where a bone of an entity is in the world, as a matrix.
pub(crate) fn bone_matrix(lua: &Lua, handle: EntityHandle, bone: usize) -> Option<Matrix> {
    let (_, bones) = model_bones(lua, handle)?;
    let world = rig_pose::multiply(&placement(lua, handle)?, bones.get(bone)?);
    Some(to_vmatrix(&world))
}

fn to_vmatrix(m: &BoneMatrix) -> Matrix {
    Matrix { m: [m[0], m[1], m[2], [0.0, 0.0, 0.0, 1.0]] }
}

/// What moves the vertices of an entity's model to where it is posed: none for a model that
/// does not move (one bone, no sequences, nothing moved by Lua).
pub(crate) fn skinning(lua: &Lua, handle: EntityHandle) -> Vec<[[f32; 4]; 3]> {
    let still = animated(lua, handle).is_none_or(|(rig, _)| rig.bones().len() <= 1 && rig.sequence_count() <= 1);
    let manipulated = host(lua).entities.get(handle).is_some_and(|e| !e.bone_manipulations.is_empty() || e.effects & EF_BONEMERGE != 0);
    if still && !manipulated {
        return Vec::new();
    }
    model_bones(lua, handle).map(|(rig, bones)| rig.skinning(&bones)).unwrap_or_default()
}

/// The view models of a player to draw, with their bones; the hands on the first when the
/// weapon in hand takes them; and the field of view they are drawn with. Their origin and
/// angles are the player's eyes.
pub(crate) fn view_models(lua: &Lua, viewer: EntityHandle) -> (Vec<crate::Drawn>, f32) {
    let fov = view_model_fov(lua, viewer);
    let Some((view_models, active, hands, show)) =
        player::state(lua, viewer, |_, p| (p.view_models, p.active_weapon, p.hands, p.alive && p.draw_view_model))
    else {
        return (Vec::new(), fov);
    };
    let Some((eye, _)) = player::eye(lua, viewer) else { return (Vec::new(), fov) };
    let angles = player::eye_angles(lua, viewer).unwrap_or_default();
    if !show || !host(lua).entities.is_valid(active) {
        return (Vec::new(), fov);
    }
    let drawn = |handle: EntityHandle| -> Option<crate::Drawn> {
        let (model, skin, hidden) = host(lua).entities.get(handle).map(|e| (e.model.clone(), e.skin, e.no_draw || e.effects & 32 != 0))?;
        if hidden || model.is_empty() {
            return None;
        }
        Some(crate::Drawn { entity: handle, model, skin, origin: eye, angles, scale: 1.0, bones: skinning(lua, handle) })
    };
    let mut out: Vec<crate::Drawn> = view_models.iter().filter_map(|&vm| drawn(vm)).collect();
    // the hands go on the first view model, when the weapon wants them (`SWEP.UseHands`, and
    // every engine weapon), as the base gamemode draws them
    let scripted = host(lua).entities.get(active).is_some_and(|e| e.scripted);
    let wants_hands = !scripted || field(lua, active, "UseHands").is_some_and(|v| !matches!(v, Value::Nil | Value::Boolean(false)));
    let on_view_model = host(lua).entities.get(hands).is_some_and(|e| e.parent == view_models[0]);
    if wants_hands && on_view_model && out.first().is_some_and(|d| d.entity == view_models[0]) {
        out.extend(drawn(hands));
    }
    (out, fov)
}

/// Where the client's Lua puts the view models this frame, from the eyes: the gamemode's
/// `CalcViewModelView`, which asks the weapon (`GetViewModelPosition`, `CalcViewModelView`:
/// iron sights, sway).
pub(crate) fn view_model_view(lua: &Lua, origin: Vector, angles: Angle) -> (Vector, Angle) {
    let player = host(lua).client.local_player.get();
    let Some(active) = player::active_weapon(lua, player).filter(|&w| host(lua).entities.is_valid(w)) else { return (origin, angles) };
    let vm = view_model(lua, player, 0);
    let values = (|| -> mlua::Result<[Value; 6]> {
        let (o, a) = (entity::vector(lua, origin)?, entity::angle(lua, angles)?);
        Ok([entity::push(lua, active)?, entity::push(lua, vm)?, o.clone(), a.clone(), o, a])
    })();
    let Ok(values) = values else { return (origin, angles) };
    let placed = crate::call_hook(lua, "CalcViewModelView", MultiValue::from_iter(values));
    let pos = match placed.front() {
        Some(Value::UserData(ud)) => ud.borrow::<Vector>().map(|v| *v).unwrap_or(origin),
        _ => origin,
    };
    let ang = match placed.get(1) {
        Some(Value::UserData(ud)) => ud.borrow::<Angle>().map(|a| *a).unwrap_or(angles),
        _ => angles,
    };
    (pos, ang)
}

/// A field of an entity's table (a SWEP's), through its metatable.
fn field(lua: &Lua, handle: EntityHandle, name: &str) -> Option<Value> {
    let Ok(Value::UserData(ud)) = entity::push(lua, handle) else { return None };
    ud.get::<Value>(name).ok()
}

/// The field of view of a player's view models: the weapon's `ViewModelFOV`, or for an
/// engine weapon `viewmodel_fov`.
fn view_model_fov(lua: &Lua, viewer: EntityHandle) -> f32 {
    let active = player::active_weapon(lua, viewer).unwrap_or(EntityHandle::NULL);
    let own = field(lua, active, "ViewModelFOV").and_then(|v| match v {
        Value::Integer(n) => Some(n as f32),
        Value::Number(n) => Some(n as f32),
        _ => None,
    });
    own.or_else(|| crate::convar::value(lua, "viewmodel_fov").and_then(|v| v.parse().ok())).unwrap_or(54.0)
}

/// A player's view model `index`.
fn view_model(lua: &Lua, owner: EntityHandle, index: usize) -> EntityHandle {
    player::state(lua, owner, |_, p| p.view_models.get(index).copied()).flatten().unwrap_or(EntityHandle::NULL)
}

/// A view model takes a weapon's model (`SetWeaponModel`); `OnViewModelChanged` hears of it.
pub(crate) fn set_weapon_model(lua: &Lua, vm: EntityHandle, model: &str, weapon: EntityHandle) {
    let Some(old) = host(lua).entities.get(vm).map(|e| e.model.clone()) else { return };
    entity::set_model(lua, vm, model);
    host(lua).entities.with_mut(vm, |ent| {
        ent.anim = AnimState::default();
        ent.links.retain(|(k, _)| k != "weapon");
        ent.links.push(("weapon".into(), weapon));
    });
    if !old.eq_ignore_ascii_case(model) {
        let (Ok(v), Ok(o), Ok(n)) = (entity::push(lua, vm), lua.create_string(&old), lua.create_string(model)) else { return };
        crate::call_hook(lua, "OnViewModelChanged", MultiValue::from_iter([v, Value::String(o), Value::String(n)]));
    }
}

/// A weapon comes out: its owner's view model shows it and plays drawing it.
pub(crate) fn deploy(lua: &Lua, owner: EntityHandle, weapon: EntityHandle) {
    let vm = view_model(lua, owner, 0);
    if !host(lua).entities.is_valid(vm) {
        return;
    }
    let model = crate::weapon::data(lua, weapon).map(|d| d.view_model).unwrap_or_default();
    set_weapon_model(lua, vm, &model, weapon);
    send_weapon_anim(lua, weapon, ACT_VM_DRAW);
}

/// `SendWeaponAnim`: the weapon's activity; the view model of its owner plays a sequence of it,
/// and the weapon goes back to idle when that has played. Whether the view model has one.
pub(crate) fn send_weapon_anim(lua: &Lua, weapon: EntityHandle, act: i64) -> bool {
    crate::weapon::set_activity(lua, weapon, act);
    let owner = host(lua).entities.get(weapon).map_or(EntityHandle::NULL, |e| e.owner);
    let vm = view_model(lua, owner, 0);
    let Some((rig, _)) = animated(lua, vm) else { return false };
    let sequence = select_weighted(lua, &rig, act);
    if sequence < 0 {
        return false;
    }
    send_view_model_sequence(lua, vm, sequence);
    true
}

/// A view model plays a sequence from its start (`SendViewModelMatchingSequence`); its weapon
/// is busy for as long as it takes.
pub(crate) fn send_view_model_sequence(lua: &Lua, vm: EntityHandle, sequence: i64) {
    play(lua, vm, sequence);
    let Some((rig, anim)) = animated(lua, vm) else { return };
    let duration = sequence_of(&rig, sequence).map_or(0.0, |s| rig.duration(s, &anim.parameters(&rig)));
    let weapon = host(lua).entities.get(vm).and_then(|e| e.links.iter().find(|(k, _)| k == "weapon").map(|&(_, w)| w));
    if let Some(weapon) = weapon {
        crate::weapon::set_idle_time(lua, weapon, now(lua) + f64::from(duration));
    }
}

/// How long the sequence a weapon's view model plays takes, in seconds.
pub(crate) fn view_model_duration(lua: &Lua, weapon: EntityHandle) -> f64 {
    let owner = host(lua).entities.get(weapon).map_or(EntityHandle::NULL, |e| e.owner);
    let vm = view_model(lua, owner, 0);
    animated(lua, vm)
        .and_then(|(rig, anim)| Some(rig.duration(sequence_of(&rig, anim.sequence)?, &anim.parameters(&rig))))
        .map_or(0.0, f64::from)
}

/// An engine weapon goes back to idle once what it sent has played and nothing is pressed, as
/// the engine's weapons do; a scripted one says for itself.
pub(crate) fn weapon_idle(lua: &Lua, weapon: EntityHandle, busy: bool) {
    let scripted = host(lua).entities.get(weapon).is_none_or(|e| e.scripted);
    let Some((activity, idle)) = crate::weapon::activity_and_idle(lua, weapon) else { return };
    if scripted || busy || activity == ACT_VM_IDLE || now(lua) < idle {
        return;
    }
    send_weapon_anim(lua, weapon, ACT_VM_IDLE);
}

/// A player's animation event (`DoAnimationEvent`): the gamemode plays the gesture of it.
pub(crate) fn animation_event(lua: &Lua, handle: EntityHandle, event: i64, data: i64) {
    let Ok(ply) = entity::push(lua, handle) else { return };
    crate::call_hook(lua, "DoAnimationEvent", MultiValue::from_iter([ply, Value::Integer(event), Value::Integer(data)]));
}

/// One tick of a player's own model: the gamemode chooses the activity and the sequence
/// (`CalcMainActivity`, `TranslateActivity`) and the rate (`UpdateAnimation`); the engine
/// turns the legs and sets the pose parameters of moving and aiming, and moves the cycle on.
pub(crate) fn animate_player(lua: &Lua, handle: EntityHandle) {
    let Some((rig, anim)) = animated(lua, handle) else { return };
    let Ok(ply) = entity::push(lua, handle) else { return };
    let velocity = host(lua).entities.get(handle).map(|e| e.velocity).unwrap_or_default();
    let Ok(v) = entity::vector(lua, velocity) else { return };
    let number = |values: &MultiValue, i: usize, default: i64| match values.get(i) {
        Some(Value::Integer(n)) => *n,
        Some(Value::Number(n)) => *n as i64,
        _ => default,
    };
    let chosen = crate::call_hook(lua, "CalcMainActivity", MultiValue::from_iter([ply.clone(), v.clone()]));
    let (ideal, forced) = (number(&chosen, 0, ACT_MP_STAND_IDLE), number(&chosen, 1, -1));
    let translated = crate::call_hook(lua, "TranslateActivity", MultiValue::from_iter([ply.clone(), Value::Integer(ideal)]));
    let act = number(&translated, 0, ideal);
    let sequence = if forced >= 0 {
        forced
    } else if act == anim.activity && sequence_of(&rig, anim.sequence).is_some() {
        anim.sequence
    } else {
        select_weighted(lua, &rig, act)
    };
    change(lua, handle, |a| a.activity = act);
    if sequence >= 0 && sequence != anim.sequence {
        reset_sequence(lua, handle, sequence);
    }
    let parameters = anim.parameters(&rig);
    let ground_speed = sequence_of(&rig, sequence).map_or(0.0, |s| rig.ground_speed(s, &parameters));
    crate::call_hook(lua, "UpdateAnimation", MultiValue::from_iter([ply, v, Value::Number(f64::from(ground_speed))]));
    aim_and_move(lua, handle, &rig, velocity, ground_speed);
    frame_advance(lua, handle, Some(crate::timer::TICK_INTERVAL));
}

/// The pose parameters the engine sets on a player: the legs turning after the eyes, `move_x`
/// and `move_y` the way it walks from where its legs point, and the aim the rest of the way.
fn aim_and_move(lua: &Lua, handle: EntityHandle, rig: &Rig, velocity: Vector, ground_speed: f32) {
    let eyes = player::eye_angles(lua, handle).unwrap_or_default();
    let speed = (velocity.x * velocity.x + velocity.y * velocity.y).sqrt();
    let difference = |a: f32, b: f32| (a - b + 180.0).rem_euclid(360.0) - 180.0;
    let find = |name: &str| rig.pose_parameters().iter().position(|p| p.name.eq_ignore_ascii_case(name));
    host(lua).entities.with_mut(handle, |ent| {
        let anim = &mut ent.anim;
        // moving, the legs go where the eyes look; standing, once the body is turned too far
        let off = difference(eyes.y, anim.feet_yaw);
        if speed > 1.0 {
            anim.feet_yaw = eyes.y;
        } else if off.abs() > 45.0 {
            anim.feet_yaw = eyes.y - 45.0 * off.signum();
        }
        let feet = anim.feet_yaw;
        let mut set = |name: &str, value: f32| {
            if let Some(i) = find(name) {
                let normalized = rig.pose_parameters()[i].normalize(value);
                anim.set_pose(i, normalized);
            }
        };
        let walking = if speed > 0.5 { (speed / ground_speed.max(1.0)).min(1.0) } else { 0.0 };
        let heading = velocity.y.atan2(velocity.x).to_degrees();
        let along = difference(heading, feet).to_radians();
        set("move_x", along.cos() * walking);
        set("move_y", -along.sin() * walking);
        set("move_yaw", if speed > 0.5 { difference(heading, feet) } else { 0.0 });
        set("aim_yaw", difference(eyes.y, feet));
        set("aim_pitch", eyes.p);
        set("head_yaw", 0.0);
        set("head_pitch", 0.0);
        set("body_yaw", difference(eyes.y, feet));
    });
    // the model is drawn turned as its legs are (`GetRenderAngles`)
    let feet = host(lua).entities.get(handle).map_or(0.0, |e| e.anim.feet_yaw);
    player::with_player(lua, handle, |_, p| p.render_angles = Angle::new(0.0, feet, 0.0));
}

/// A player's gesture slot plays a sequence over the rest: the layer of the slot, from its
/// start.
pub(crate) fn restart_gesture(lua: &Lua, handle: EntityHandle, slot: i64, sequence: i64, activity: i64, autokill: bool) {
    change(lua, handle, |anim| {
        anim.layers.retain(|l| l.slot != slot);
        let Some(id) = (0..MAX_LAYERS).find(|id| anim.layers.iter().all(|l| l.id != *id)) else { return };
        let mut layer = AnimLayer::new(id, sequence);
        layer.slot = slot;
        layer.activity = activity;
        layer.autokill = autokill;
        anim.layers.push(layer);
    });
}

// ---------------------------------------------------------------------------------------------
// The methods of `Entity`

fn this(a: &Args) -> mlua::Result<EntityHandle> {
    entity::this(a, 1)
}

/// The rig and the animation of the entity in argument 1; no rig without a studio model.
fn of(a: &Args) -> mlua::Result<(EntityHandle, Option<Arc<Rig>>, AnimState)> {
    let handle = this(a)?;
    let (model, anim) = host(a.lua).entities.get(handle).map(|e| (e.model.clone(), e.anim.clone())).expect("checked above");
    Ok((handle, rig(a.lua, &model), anim))
}

/// A sequence named by argument `position`: a number, or a name.
fn sequence_arg(a: &Args, rig: Option<&Rig>, position: usize) -> mlua::Result<i64> {
    match a.get(position) {
        Some(Value::String(s)) => {
            let name = s.to_string_lossy();
            Ok(rig.and_then(|r| r.find_sequence(&name)).map_or(-1, |s| s as i64))
        }
        _ => Ok(a.number(position)? as i64),
    }
}

/// A pose parameter named by argument 2: a number, or a name.
fn pose_arg(a: &Args, rig: &Rig) -> mlua::Result<Option<usize>> {
    Ok(match a.get(2) {
        Some(Value::String(s)) => {
            let name = s.to_string_lossy();
            rig.pose_parameters().iter().position(|p| p.name.eq_ignore_ascii_case(&name))
        }
        _ => usize::try_from(a.number(2)? as i64).ok().filter(|&i| i < rig.pose_parameters().len()),
    })
}

fn number(n: f64) -> mlua::Result<Value> {
    Ok(Value::Number(n))
}

fn integer(n: i64) -> mlua::Result<Value> {
    Ok(Value::Integer(n))
}

fn string(lua: &Lua, text: &str) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(text)?))
}

/// A layer of the entity in argument 1 by the number in argument 2.
fn layer_arg(a: &Args) -> mlua::Result<(EntityHandle, usize)> {
    Ok((this(a)?, a.number(2)? as i64 as usize))
}

fn read_layer<R>(a: &Args, f: impl FnOnce(&AnimLayer) -> R, default: R) -> mlua::Result<R> {
    let (handle, id) = layer_arg(a)?;
    Ok(host(a.lua).entities.get(handle).and_then(|e| e.anim.layers.iter().find(|l| l.id == id).map(f)).unwrap_or(default))
}

fn write_layer(a: &Args, f: impl FnOnce(&mut AnimLayer)) -> mlua::Result<Value> {
    let (handle, id) = layer_arg(a)?;
    change(a.lua, handle, |anim| {
        if let Some(layer) = anim.layers.iter_mut().find(|l| l.id == id) {
            f(layer);
        }
    });
    Ok(Value::Nil)
}

/// Adds a layer playing `sequence` from its start; its number, or -1 when there is no room.
fn add_layer(lua: &Lua, handle: EntityHandle, sequence: i64, f: impl FnOnce(&mut AnimLayer)) -> i64 {
    let mut added = -1;
    change(lua, handle, |anim| {
        if let Some(id) = (0..MAX_LAYERS).find(|id| anim.layers.iter().all(|l| l.id != *id)) {
            let mut layer = AnimLayer::new(id, sequence);
            f(&mut layer);
            anim.layers.push(layer);
            added = id as i64;
        }
    });
    added
}

/// What `GetAttachment` gives: where an attachment is (counted from 1) and its bone; nil for
/// none.
fn attachment(lua: &Lua, handle: EntityHandle, index: i64) -> mlua::Result<Value> {
    let Some((rig, bones)) = model_bones(lua, handle) else { return Ok(Value::Nil) };
    let Some(local) = usize::try_from(index - 1).ok().and_then(|i| rig.attachment(i, &bones)) else { return Ok(Value::Nil) };
    let Some(place) = placement(lua, handle) else { return Ok(Value::Nil) };
    let world = to_vmatrix(&rig_pose::multiply(&place, &local));
    let t = lua.create_table()?;
    t.raw_set("Pos", entity::vector(lua, world.translation())?)?;
    t.raw_set("Ang", entity::angle(lua, world.angles())?)?;
    t.raw_set("Bone", rig.attachments().get((index - 1) as usize).map_or(-1, |a| a.bone as i64))?;
    Ok(Value::Table(t))
}

pub(crate) fn install_methods(def: &Def) -> mlua::Result<()> {
    // the sequence and its cycle
    def("GetSequence", |a| integer(of(a)?.2.sequence))?;
    def("SetSequence", |a| {
        let (handle, rig, _) = of(a)?;
        let sequence = sequence_arg(a, rig.as_deref(), 2)?;
        change(a.lua, handle, |anim| anim.sequence = sequence);
        Ok(Value::Nil)
    })?;
    def("ResetSequence", |a| {
        let (handle, rig, _) = of(a)?;
        let sequence = sequence_arg(a, rig.as_deref(), 2)?;
        reset_sequence(a.lua, handle, sequence);
        Ok(Value::Nil)
    })?;
    def("ResetSequenceInfo", |a| {
        let handle = this(a)?;
        let now = now(a.lua);
        change(a.lua, handle, |anim| {
            anim.finished = false;
            anim.anim_time = now;
        });
        Ok(Value::Nil)
    })?;
    def("GetCycle", |a| number(f64::from(of(a)?.2.cycle)))?;
    def("SetCycle", |a| {
        let (handle, cycle) = (this(a)?, a.number(2)? as f32);
        change(a.lua, handle, |anim| anim.cycle = cycle.clamp(0.0, 1.0));
        Ok(Value::Nil)
    })?;
    def("GetPlaybackRate", |a| number(f64::from(of(a)?.2.playback_rate)))?;
    def("SetPlaybackRate", |a| {
        let (handle, rate) = (this(a)?, a.number(2)? as f32);
        change(a.lua, handle, |anim| anim.playback_rate = rate);
        Ok(Value::Nil)
    })?;
    def("GetAnimTime", |a| number(of(a)?.2.anim_time))?;
    def("SetAnimTime", |a| {
        let (handle, time) = (this(a)?, a.number(2)?);
        change(a.lua, handle, |anim| anim.anim_time = time);
        Ok(Value::Nil)
    })?;
    def("GetAnimTimeInterval", |a| {
        let anim = of(a)?.2;
        number(if anim.anim_time > 0.0 { (now(a.lua) - anim.anim_time).max(0.0) } else { 0.0 })
    })?;
    def("FrameAdvance", |a| {
        let handle = this(a)?;
        frame_advance(a.lua, handle, a.opt_number(2)?);
        Ok(Value::Nil)
    })?;
    def("IsSequenceFinished", |a| Ok(Value::Boolean(of(a)?.2.finished)))?;
    // the client animates what the server sends it: there is nothing more to choose here
    def("UseClientSideAnimation", |a| this(a).map(|_| Value::Nil))?;
    def("SetupBones", |a| this(a).map(|_| Value::Nil))?;
    def("InvalidateBoneCache", |a| this(a).map(|_| Value::Nil))?;

    // the sequences of the model
    def("GetSequenceCount", |a| integer(of(a)?.1.map_or(0, |r| r.sequence_count() as i64)))?;
    def("GetSequenceName", |a| {
        let (_, rig, _) = of(a)?;
        let sequence = a.number(2)? as i64;
        let label = rig.as_deref().and_then(|r| r.sequence(sequence_of(r, sequence)?).map(|s| s.label.clone()));
        string(a.lua, label.as_deref().unwrap_or("Unknown"))
    })?;
    def("GetSequenceList", |a| {
        let (_, rig, _) = of(a)?;
        let list = a.lua.create_table()?;
        if let Some(rig) = rig {
            // numbered from 0, as the sequences are
            for s in 0..rig.sequence_count() {
                list.raw_set(s, rig.sequence(s).map(|q| q.label.clone()).unwrap_or_default())?;
            }
        }
        Ok(Value::Table(list))
    })?;
    def("GetSequenceActivity", |a| {
        let (_, rig, _) = of(a)?;
        let sequence = a.number(2)? as i64;
        let name = rig.as_deref().and_then(|r| r.sequence(sequence_of(r, sequence)?).map(|s| s.activity.clone()));
        integer(name.map_or(-1, |n| activity_id(a.lua, &n)))
    })?;
    def("GetSequenceActivityName", |a| {
        let (_, rig, _) = of(a)?;
        let sequence = a.number(2)? as i64;
        let name = rig.as_deref().and_then(|r| r.sequence(sequence_of(r, sequence)?).map(|s| s.activity.clone()));
        string(a.lua, name.as_deref().unwrap_or(""))
    })?;
    def("SelectWeightedSequence", |a| {
        let (_, rig, _) = of(a)?;
        let act = a.number(2)? as i64;
        integer(rig.map_or(-1, |r| select_weighted(a.lua, &r, act)))
    })?;
    def("SelectWeightedSequenceSeeded", |a| {
        let (_, rig, _) = of(a)?;
        let (act, seed) = (a.number(2)? as i64, a.number(3)? as i64);
        integer(rig.map_or(-1, |r| weighted(a.lua, &r, act, |total| seed.rem_euclid(total))))
    })?;
    def("FindTransitionSequence", |a| {
        this(a)?;
        integer(a.number(3)? as i64)
    })?;
    def("SequenceDuration", |a| {
        let (_, rig, anim) = of(a)?;
        let sequence = a.opt_number(2)?.map_or(anim.sequence, |s| s as i64);
        number(rig.and_then(|r| Some(f64::from(r.duration(sequence_of(&r, sequence)?, &anim.parameters(&r))))).unwrap_or(0.0))
    })?;
    def("GetSequenceGroundSpeed", |a| {
        let (_, rig, anim) = of(a)?;
        let sequence = a.number(2)? as i64;
        number(rig.and_then(|r| Some(f64::from(r.ground_speed(sequence_of(&r, sequence)?, &anim.parameters(&r))))).unwrap_or(0.0))
    })?;
    def("GetSequenceMoveDist", |a| {
        let (_, rig, anim) = of(a)?;
        let sequence = a.number(2)? as i64;
        let moved = rig.and_then(|r| Some(r.movement(sequence_of(&r, sequence)?, &anim.parameters(&r)))).unwrap_or_default();
        number(f64::from((moved[0] * moved[0] + moved[1] * moved[1] + moved[2] * moved[2]).sqrt()))
    })?;
    def("GetSequenceMoveYaw", |a| {
        let (_, rig, anim) = of(a)?;
        let sequence = a.number(2)? as i64;
        let moved = rig.and_then(|r| Some(r.movement(sequence_of(&r, sequence)?, &anim.parameters(&r)))).unwrap_or_default();
        number(if moved[0] == 0.0 && moved[1] == 0.0 { 0.0 } else { f64::from(moved[1].atan2(moved[0]).to_degrees()) })
    })?;
    def("GetSequenceVelocity", |a| {
        let (_, rig, anim) = of(a)?;
        let sequence = a.number(2)? as i64;
        let velocity = rig
            .and_then(|r| {
                let s = sequence_of(&r, sequence)?;
                let parameters = anim.parameters(&r);
                let (moved, rate) = (r.movement(s, &parameters), r.cycle_rate(s, &parameters));
                Some(Vector::new(moved[0] * rate, moved[1] * rate, moved[2] * rate))
            })
            .unwrap_or_default();
        entity::vector(a.lua, velocity)
    })?;
    def("GetSequenceInfo", |a| {
        let (_, rig, anim) = of(a)?;
        let sequence = a.number(2)? as i64;
        let Some(rig) = rig else { return Ok(Value::Nil) };
        let Some(seq) = sequence_of(&rig, sequence).and_then(|s| rig.sequence(s)) else { return Ok(Value::Nil) };
        let t = a.lua.create_table()?;
        t.raw_set("label", seq.label.as_str())?;
        t.raw_set("activityname", seq.activity.as_str())?;
        t.raw_set("activity", activity_id(a.lua, &seq.activity))?;
        t.raw_set("actweight", seq.activity_weight)?;
        t.raw_set("flags", seq.flags)?;
        t.raw_set("numevents", seq.events.len())?;
        t.raw_set("numblends", seq.animations.len())?;
        let (lo, hi) = seq.bounds;
        t.raw_set("bbmin", entity::vector(a.lua, Vector::new(lo[0], lo[1], lo[2]))?)?;
        t.raw_set("bbmax", entity::vector(a.lua, Vector::new(hi[0], hi[1], hi[2]))?)?;
        t.raw_set("fadeintime", seq.fade_in)?;
        t.raw_set("fadeouttime", seq.fade_out)?;
        let anims = a.lua.create_sequence_from(seq.animations.iter().copied())?;
        t.raw_set("anims", anims)?;
        let duration = rig.duration(sequence as usize, &anim.parameters(&rig));
        t.raw_set("lastframe", duration)?;
        Ok(Value::Table(t))
    })?;
    def("GetAnimCount", |a| integer(of(a)?.1.map_or(0, |r| r.mdl().animations.len() as i64)))?;
    def("GetAnimInfo", |a| {
        let (_, rig, _) = of(a)?;
        let index = a.number(2)? as i64;
        let Some(anim) = rig.as_deref().and_then(|r| r.mdl().animations.get(usize::try_from(index).ok()?).cloned()) else {
            return Ok(Value::Nil);
        };
        let t = a.lua.create_table()?;
        t.raw_set("label", anim.name.as_str())?;
        t.raw_set("fps", anim.fps)?;
        t.raw_set("flags", anim.flags)?;
        t.raw_set("numframes", anim.frames)?;
        Ok(Value::Table(t))
    })?;

    // pose parameters: kept from 0 to 1 across their ranges, set and (on a client) given in
    // their own units; a server gives them as kept, as GMod's does
    def("GetNumPoseParameters", |a| integer(of(a)?.1.map_or(0, |r| r.pose_parameters().len() as i64)))?;
    def("GetPoseParameterName", |a| {
        let (_, rig, _) = of(a)?;
        let index = a.number(2)? as i64;
        let name = rig.as_deref().and_then(|r| r.pose_parameters().get(usize::try_from(index).ok()?).map(|p| p.name.clone()));
        string(a.lua, name.as_deref().unwrap_or(""))
    })?;
    def("LookupPoseParameter", |a| {
        let (_, rig, _) = of(a)?;
        let name = a.string(2)?;
        integer(rig.and_then(|r| r.pose_parameters().iter().position(|p| p.name.eq_ignore_ascii_case(&name))).map_or(-1, |i| i as i64))
    })?;
    def("SetPoseParameter", |a| {
        let (handle, rig, _) = of(a)?;
        let Some(rig) = rig else { return Ok(Value::Nil) };
        let value = a.number(3)? as f32;
        if let Some(index) = pose_arg(a, &rig)? {
            let pose = &rig.pose_parameters()[index];
            let mut value = value;
            if pose.wrap != 0.0 {
                let middle = (pose.start + pose.end) / 2.0;
                value -= pose.wrap * ((value - middle) / pose.wrap + 0.5).floor();
            }
            let normalized = pose.normalize(value);
            change(a.lua, handle, |anim| anim.set_pose(index, normalized));
        }
        Ok(Value::Nil)
    })?;
    def("GetPoseParameter", |a| {
        let (_, rig, anim) = of(a)?;
        let Some(rig) = rig else { return number(0.0) };
        let Some(index) = pose_arg(a, &rig)? else { return number(0.0) };
        let normalized = anim.parameters(&rig)[index];
        let value = if host(a.lua).realm == crate::Realm::Server { normalized } else { rig.pose_parameters()[index].value(normalized) };
        number(f64::from(value))
    })?;
    def("ClearPoseParameters", |a| {
        let handle = this(a)?;
        change(a.lua, handle, |anim| anim.pose.clear());
        Ok(Value::Nil)
    })?;

    // layers over the sequence
    def("AddGesture", |a| {
        let (handle, rig, _) = of(a)?;
        let (act, autokill) = (a.number(2)? as i64, a.get(3).is_none() || a.boolean(3));
        let sequence = rig.map_or(-1, |r| select_weighted(a.lua, &r, act));
        if sequence < 0 {
            return integer(-1);
        }
        integer(add_layer(a.lua, handle, sequence, |l| {
            l.activity = act;
            l.autokill = autokill;
        }))
    })?;
    def("AddGestureSequence", |a| {
        let handle = this(a)?;
        let (sequence, autokill) = (a.number(2)? as i64, a.get(3).is_none() || a.boolean(3));
        integer(add_layer(a.lua, handle, sequence, |l| l.autokill = autokill))
    })?;
    def("AddLayeredSequence", |a| {
        let handle = this(a)?;
        let (sequence, priority) = (a.number(2)? as i64, a.opt_number(3)?.unwrap_or(0.0) as i64);
        integer(add_layer(a.lua, handle, sequence, |l| l.priority = priority))
    })?;
    def("RestartGesture", |a| {
        let (handle, rig, anim) = of(a)?;
        let (act, add, autokill) = (a.number(2)? as i64, a.get(3).is_none() || a.boolean(3), a.get(4).is_none() || a.boolean(4));
        if anim.layers.iter().any(|l| l.activity == act) {
            change(a.lua, handle, |anim| {
                for layer in anim.layers.iter_mut().filter(|l| l.activity == act) {
                    layer.cycle = 0.0;
                    layer.finished = false;
                }
            });
        } else if add && let Some(sequence) = rig.map(|r| select_weighted(a.lua, &r, act)).filter(|&s| s >= 0) {
            add_layer(a.lua, handle, sequence, |l| {
                l.activity = act;
                l.autokill = autokill;
            });
        }
        Ok(Value::Nil)
    })?;
    def("RemoveGesture", |a| {
        let (handle, act) = (this(a)?, a.number(2)? as i64);
        change(a.lua, handle, |anim| anim.layers.retain(|l| l.activity != act));
        Ok(Value::Nil)
    })?;
    def("RemoveAllGestures", |a| {
        let handle = this(a)?;
        change(a.lua, handle, |anim| anim.layers.retain(|l| l.activity < 0));
        Ok(Value::Nil)
    })?;
    def("FindGestureLayer", |a| {
        let (_, _, anim) = of(a)?;
        let act = a.number(2)? as i64;
        integer(anim.layers.iter().find(|l| l.activity == act).map_or(-1, |l| l.id as i64))
    })?;
    def("FindGestureSequenceLayer", |a| {
        let (_, _, anim) = of(a)?;
        let sequence = a.number(2)? as i64;
        integer(anim.layers.iter().find(|l| l.sequence == sequence).map_or(-1, |l| l.id as i64))
    })?;
    def("IsPlayingGesture", |a| {
        let (_, _, anim) = of(a)?;
        let act = a.number(2)? as i64;
        Ok(Value::Boolean(anim.layers.iter().any(|l| l.activity == act)))
    })?;
    def("IsValidLayer", |a| Ok(Value::Boolean(read_layer(a, |_| true, false)?)))?;
    def("RemoveLayer", |a| {
        let (handle, id) = layer_arg(a)?;
        change(a.lua, handle, |anim| anim.layers.retain(|l| l.id != id));
        Ok(Value::Nil)
    })?;
    def("GetLayerCycle", |a| number(f64::from(read_layer(a, |l| l.cycle, 0.0)?)))?;
    def("SetLayerCycle", |a| {
        let cycle = a.number(3)? as f32;
        write_layer(a, |l| l.cycle = cycle.clamp(0.0, 1.0))
    })?;
    def("GetLayerWeight", |a| number(f64::from(read_layer(a, |l| l.weight, 0.0)?)))?;
    def("SetLayerWeight", |a| {
        let weight = a.number(3)? as f32;
        write_layer(a, |l| l.weight = weight.clamp(0.0, 1.0))
    })?;
    def("GetLayerPlaybackRate", |a| number(f64::from(read_layer(a, |l| l.playback_rate, 0.0)?)))?;
    def("SetLayerPlaybackRate", |a| {
        let rate = a.number(3)? as f32;
        write_layer(a, |l| l.playback_rate = rate)
    })?;
    def("GetLayerSequence", |a| integer(read_layer(a, |l| l.sequence, -1)?))?;
    def("SetLayerSequence", |a| {
        let sequence = a.number(3)? as i64;
        write_layer(a, |l| {
            l.sequence = sequence;
            l.cycle = 0.0;
            l.finished = false;
        })
    })?;
    def("SetLayerLooping", |a| {
        let looping = a.boolean(3);
        write_layer(a, |l| l.looping = looping)
    })?;
    def("SetLayerAutokill", |a| {
        let autokill = a.boolean(3);
        write_layer(a, |l| l.autokill = autokill)
    })?;
    def("SetLayerPriority", |a| {
        let priority = a.number(3)? as i64;
        write_layer(a, |l| l.priority = priority)
    })?;
    def("SetLayerBlendIn", |a| {
        let blend = a.number(3)? as f32;
        write_layer(a, |l| l.blend_in = blend.clamp(0.0, 1.0))
    })?;
    def("SetLayerBlendOut", |a| {
        let blend = a.number(3)? as f32;
        write_layer(a, |l| l.blend_out = blend.clamp(0.0, 1.0))
    })?;
    // a layer playing for `duration` seconds plays at the rate that takes
    def("SetLayerDuration", |a| {
        let (handle, id) = layer_arg(a)?;
        let duration = a.number(3)? as f32;
        let (_, rig, anim) = of(a)?;
        let own = anim.layers.iter().find(|l| l.id == id).and_then(|l| {
            let r = rig.as_deref()?;
            Some(r.duration(sequence_of(r, l.sequence)?, &anim.parameters(r)))
        });
        if let Some(own) = own.filter(|_| duration > 0.0) {
            change(a.lua, handle, |anim| {
                if let Some(l) = anim.layers.iter_mut().find(|l| l.id == id) {
                    l.playback_rate = own / duration;
                }
            });
        }
        Ok(Value::Nil)
    })?;
    def("GetLayerDuration", |a| {
        let (_, rig, anim) = of(a)?;
        let id = a.number(2)? as i64 as usize;
        let duration = anim.layers.iter().find(|l| l.id == id).and_then(|l| {
            let r = rig.as_deref()?;
            let own = r.duration(sequence_of(r, l.sequence)?, &anim.parameters(r));
            Some(if l.playback_rate > 0.0 { own / l.playback_rate } else { own })
        });
        number(f64::from(duration.unwrap_or(0.0)))
    })?;

    // bones and attachments where they are now
    def("GetBoneMatrix", |a| {
        let handle = this(a)?;
        let bone = a.number(2)? as i64;
        match usize::try_from(bone).ok().and_then(|b| bone_matrix(a.lua, handle, b)) {
            Some(m) => Ok(Value::UserData(a.lua.create_userdata(m)?)),
            None => Ok(Value::Nil),
        }
    })?;
    def("BoneHasFlag", |a| {
        let (_, rig, _) = of(a)?;
        let (bone, flag) = (a.number(2)? as i64, a.number(3)? as i64 as i32);
        let flags = rig.as_deref().and_then(|r| r.bones().get(usize::try_from(bone).ok()?).map(|b| b.flags)).unwrap_or(0);
        Ok(Value::Boolean(flags & flag != 0))
    })?;
    def("GetChildBones", |a| {
        let (_, rig, _) = of(a)?;
        let bone = a.number(2)? as i64;
        let children: Vec<i64> = rig.map_or(Vec::new(), |r| {
            r.bones().iter().enumerate().filter(|(_, b)| i64::from(b.parent) == bone).map(|(i, _)| i as i64).collect()
        });
        Ok(Value::Table(a.lua.create_sequence_from(children)?))
    })?;
    def("BoneLength", |a| {
        let handle = this(a)?;
        let bone = a.number(2)? as i64;
        let length = model_bones(a.lua, handle).and_then(|(rig, bones)| {
            let b = usize::try_from(bone).ok()?;
            let parent = usize::try_from(rig.bones().get(b)?.parent).ok()?;
            let (p, q) = (rig_pose::transform(bones.get(b)?, [0.0; 3]), rig_pose::transform(bones.get(parent)?, [0.0; 3]));
            Some(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt())
        });
        number(f64::from(length.unwrap_or(0.0)))
    })?;
    def("GetBoneSurfaceProp", |a| {
        let (_, rig, _) = of(a)?;
        let bone = a.number(2)? as i64;
        let name = rig.as_deref().and_then(|r| r.bones().get(usize::try_from(bone).ok()?).map(|b| b.surface_prop.clone()));
        string(a.lua, name.as_deref().unwrap_or(""))
    })?;
    def("GetBoneContents", |a| {
        let (_, rig, _) = of(a)?;
        let bone = a.number(2)? as i64;
        integer(rig.as_deref().and_then(|r| r.bones().get(usize::try_from(bone).ok()?).map(|b| i64::from(b.contents))).unwrap_or(0))
    })?;
    def("GetAttachment", |a| {
        let handle = this(a)?;
        attachment(a.lua, handle, a.number(2)? as i64)
    })?;

    // view models
    def("SendViewModelMatchingSequence", |a| {
        let handle = this(a)?;
        send_view_model_sequence(a.lua, handle, a.number(2)? as i64);
        Ok(Value::Nil)
    })?;
    def("SetWeaponModel", |a| {
        let (handle, model) = (this(a)?, a.string(2)?);
        let weapon = entity::handle_of(a.get(3)).unwrap_or(EntityHandle::NULL);
        set_weapon_model(a.lua, handle, &model, weapon);
        Ok(Value::Nil)
    })?;
    def("ViewModelIndex", |a| {
        let handle = this(a)?;
        let owner = host(a.lua).entities.get(handle).map_or(EntityHandle::NULL, |e| e.owner);
        let index = player::state(a.lua, owner, |_, p| p.view_models.iter().position(|&vm| vm == handle)).flatten();
        integer(index.map_or(0, |i| i as i64))
    })?;
    Ok(())
}

pub(crate) fn install_many(def: &DefMany) -> mlua::Result<()> {
    def("LookupSequence", |a| {
        let (_, rig, anim) = of(a)?;
        let name = a.string(2)?;
        let found = rig.as_deref().and_then(|r| r.find_sequence(&name).map(|s| (s, r.duration(s, &anim.parameters(r)))));
        Ok(match found {
            Some((s, duration)) => MultiValue::from_iter([Value::Integer(s as i64), Value::Number(f64::from(duration))]),
            None => MultiValue::from_iter([Value::Integer(-1), Value::Number(0.0)]),
        })
    })?;
    def("GetPoseParameterRange", |a| {
        let (_, rig, _) = of(a)?;
        let index = a.number(2)? as i64;
        let range = rig.as_deref().and_then(|r| r.pose_parameters().get(usize::try_from(index).ok()?).map(|p| (p.start, p.end)));
        Ok(match range {
            Some((start, end)) => MultiValue::from_iter([Value::Number(f64::from(start)), Value::Number(f64::from(end))]),
            None => MultiValue::new(),
        })
    })?;
    def("GetBonePosition", |a| {
        let handle = this(a)?;
        let bone = a.number(2)? as i64;
        let Some(m) = usize::try_from(bone).ok().and_then(|b| bone_matrix(a.lua, handle, b)) else { return Ok(MultiValue::new()) };
        Ok(MultiValue::from_iter([entity::vector(a.lua, m.translation())?, entity::angle(a.lua, m.angles())?]))
    })?;
    def("GetSequenceMovement", |a| {
        let (_, rig, anim) = of(a)?;
        let (sequence, from, to) = (a.number(2)? as i64, a.number(3)? as f32, a.number(4)? as f32);
        let moved = rig.as_deref().and_then(|r| {
            let s = sequence_of(r, sequence)?;
            let whole = r.movement(s, &anim.parameters(r));
            Some(whole.map(|v| v * (to - from)))
        });
        Ok(match moved {
            Some(m) => MultiValue::from_iter([
                Value::Boolean(true),
                entity::vector(a.lua, Vector::new(m[0], m[1], m[2]))?,
                entity::angle(a.lua, Angle::default())?,
            ]),
            None => MultiValue::from_iter([Value::Boolean(false)]),
        })
    })?;
    Ok(())
}

/// What players have of animation: their gesture slots and their animation events.
pub(crate) fn install_player(def: &Def) -> mlua::Result<()> {
    def("AnimRestartGesture", |a| {
        let (handle, rig, _) = of(a)?;
        let (slot, act, autokill) = (a.number(2)? as i64, a.number(3)? as i64, a.boolean(4));
        if let Some(sequence) = rig.map(|r| select_weighted(a.lua, &r, act)).filter(|&s| s >= 0) {
            restart_gesture(a.lua, handle, slot, sequence, act, autokill);
        }
        Ok(Value::Nil)
    })?;
    def("AnimSetGestureSequence", |a| {
        let handle = this(a)?;
        let (slot, sequence) = (a.number(2)? as i64, a.number(3)? as i64);
        restart_gesture(a.lua, handle, slot, sequence, -1, false);
        Ok(Value::Nil)
    })?;
    def("AddVCDSequenceToGestureSlot", |a| {
        let handle = this(a)?;
        let (slot, sequence, cycle, autokill) =
            (a.number(2)? as i64, a.number(3)? as i64, a.opt_number(4)?.unwrap_or(0.0) as f32, a.boolean(5));
        restart_gesture(a.lua, handle, slot, sequence, -1, autokill);
        change(a.lua, handle, |anim| {
            if let Some(layer) = anim.layers.iter_mut().find(|l| l.slot == slot) {
                layer.cycle = cycle.clamp(0.0, 1.0);
            }
        });
        Ok(Value::Nil)
    })?;
    def("AnimResetGestureSlot", |a| {
        let (handle, slot) = (this(a)?, a.number(2)? as i64);
        change(a.lua, handle, |anim| anim.layers.retain(|l| l.slot != slot));
        Ok(Value::Nil)
    })?;
    def("AnimSetGestureWeight", |a| {
        let (handle, slot, weight) = (this(a)?, a.number(2)? as i64, a.number(3)? as f32);
        change(a.lua, handle, |anim| {
            for layer in anim.layers.iter_mut().filter(|l| l.slot == slot) {
                layer.weight = weight.clamp(0.0, 1.0);
            }
        });
        Ok(Value::Nil)
    })?;
    def("AnimRestartMainSequence", |a| {
        let handle = this(a)?;
        let now = now(a.lua);
        change(a.lua, handle, |anim| {
            anim.cycle = 0.0;
            anim.finished = false;
            anim.anim_time = now;
        });
        Ok(Value::Nil)
    })?;
    def("DoAnimationEvent", |a| {
        let handle = this(a)?;
        animation_event(a.lua, handle, a.number(2)? as i64, a.opt_number(3)?.unwrap_or(0.0) as i64);
        Ok(Value::Nil)
    })?;
    def("DoAttackEvent", |a| {
        animation_event(a.lua, this(a)?, EVENT_ATTACK_PRIMARY, 0);
        Ok(Value::Nil)
    })?;
    def("DoSecondaryAttack", |a| {
        animation_event(a.lua, this(a)?, EVENT_ATTACK_SECONDARY, 0);
        Ok(Value::Nil)
    })?;
    def("DoReloadEvent", |a| {
        animation_event(a.lua, this(a)?, EVENT_RELOAD, 0);
        Ok(Value::Nil)
    })?;
    def("DoCustomAnimEvent", |a| {
        let handle = this(a)?;
        animation_event(a.lua, handle, EVENT_CUSTOM, a.opt_number(2)?.unwrap_or(0.0) as i64);
        Ok(Value::Nil)
    })?;
    // `PLAYER_ATTACK1`, `PLAYER_RELOAD`, `PLAYER_JUMP`: the events of what the player does
    def("SetAnimation", |a| {
        let handle = this(a)?;
        let event = match a.number(2)? as i64 {
            5 => Some(EVENT_ATTACK_PRIMARY),
            7 => Some(EVENT_RELOAD),
            2 | 3 => Some(EVENT_JUMP),
            _ => None,
        };
        if let Some(event) = event {
            animation_event(a.lua, handle, event, 0);
        }
        Ok(Value::Nil)
    })?;
    Ok(())
}

/// `util.GetActivityIDByName` and `util.GetActivityNameByID`.
pub(crate) fn install_util(lua: &Lua) -> mlua::Result<()> {
    let util: mlua::Table = lua.globals().get("util")?;
    util.raw_set(
        "GetActivityIDByName",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "GetActivityIDByName");
            Ok(activity_id(lua, &a.string(1)?))
        })?,
    )?;
    util.raw_set(
        "GetActivityNameByID",
        lua.create_function(|lua, values: MultiValue| {
            let a = Args::new(lua, values, "GetActivityNameByID");
            Ok(activity_name(lua, a.number(1)? as i64).unwrap_or_default())
        })?,
    )?;
    Ok(())
}
