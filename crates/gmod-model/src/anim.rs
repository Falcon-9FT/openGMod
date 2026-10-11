//! What a model plays on its bones (`.mdl`, and the animation blocks of its `.ani`):
//! sequences, the animations they blend, pose parameters, and the frames of each animation.
//!
//! A sequence is what an entity plays (`idle01`, `fire01`): an activity (`ACT_VM_IDLE`), events,
//! layers played on top of it, and a grid of animations blended by up to two pose parameters
//! (`move_x`, `aim_pitch`). An animation is the motion itself: frames at a rate, each giving
//! bones a position and a rotation relative to their parents.
//!
//! The frames are a list of records, one for each bone that the animation moves: the bone, how
//! its values are kept, and the offset to the next record. A rotation is a quaternion packed
//! into 48 or 64 bits that holds for every frame, or three angles (Euler, radians) that change;
//! a position is three half floats, or three values that change. Values that change come in
//! runs of frames: each run says how many frames it covers and how many values it keeps, the
//! last kept value holding for the rest of the run. A value is scaled by the bone's scale and,
//! unless the record is a delta, added to the bone's own rest value. Long animations are cut
//! into sections of frames, each with records of its own; animations kept outside the `.mdl`
//! are in a block of its `.ani` file.

use std::io;

use crate::mdl::Bone;
use crate::reader::{Reader, invalid};

const SEQUENCE_SIZE: usize = 212;
const ANIMATION_SIZE: usize = 100;
const POSE_PARAMETER_SIZE: usize = 20;
const EVENT_SIZE: usize = 80;
const LAYER_SIZE: usize = 24;
const MOVEMENT_SIZE: usize = 44;

/// Flags of a sequence and of an animation.
pub mod flags {
    /// It starts over when it ends.
    pub const LOOPING: i32 = 0x0001;
    /// It is added to what is played under it, not played instead of it.
    pub const DELTA: i32 = 0x0004;
    /// Added in the bone's own space rather than its parent's.
    pub const POST: i32 = 0x0010;
    /// Its cycle is a pose parameter's value ([`Sequence::cycle_pose`](super::Sequence::cycle_pose)).
    pub const CYCLE_POSE: i32 = 0x0080;
    /// Not listed for players to choose.
    pub const HIDDEN: i32 = 0x0400;
    /// It takes the place of a sequence of the same name in a model included before it.
    pub const OVERRIDE: i32 = 0x0800;
}

/// Flags of a layer a sequence plays on top of itself.
pub mod layer_flags {
    pub const SPLINE: i32 = 0x0040;
    pub const CROSS_FADE: i32 = 0x0080;
    pub const NO_BLEND: i32 = 0x0200;
    /// Played on the sequence's own pose before it is blended in.
    pub const LOCAL: i32 = 0x1000;
    /// Its weight goes with a pose parameter rather than the sequence's cycle.
    pub const POSE: i32 = 0x4000;
}

/// How a bone's record keeps its values.
mod record {
    pub const RAW_POSITION: u8 = 0x01;
    pub const RAW_ROTATION: u8 = 0x02;
    pub const POSITION: u8 = 0x04;
    pub const ROTATION: u8 = 0x08;
    pub const DELTA: u8 = 0x10;
    pub const RAW_ROTATION_64: u8 = 0x20;
}

/// A value that blends sequences: `move_x`, `aim_yaw`. Entities keep it from 0 to 1 across
/// `start..end`.
#[derive(Debug, Clone, PartialEq)]
pub struct PoseParameter {
    pub name: String,
    pub flags: i32,
    pub start: f32,
    pub end: f32,
    /// For angles that go round: the span after which a value is the same again (360), or 0.
    pub wrap: f32,
}

impl PoseParameter {
    /// A value in the parameter's units, from 0 to 1 across its range.
    pub fn normalize(&self, value: f32) -> f32 {
        if self.end == self.start { 0.0 } else { ((value - self.start) / (self.end - self.start)).clamp(0.0, 1.0) }
    }

    /// A value from 0 to 1 in the parameter's units.
    pub fn value(&self, normalized: f32) -> f32 {
        self.start + normalized * (self.end - self.start)
    }
}

/// Something a sequence makes happen at a point of its cycle: a sound, a muzzle flash.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub cycle: f32,
    /// Its number (`5001`: a muzzle flash); 0 for one given by name only.
    pub event: i32,
    pub kind: i32,
    pub options: String,
    pub name: String,
}

/// A sequence played on top of another as part of it: aim layers, breathing.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoLayer {
    /// The sequence, of the same model.
    pub sequence: usize,
    /// The pose parameter that drives it, for a [`layer_flags::POSE`] layer.
    pub pose: Option<usize>,
    pub flags: i32,
    /// Over the cycle (or the pose parameter): where it starts, is at full weight, starts to
    /// fade and is gone. All the same: it is there all the time.
    pub start: f32,
    pub peak: f32,
    pub tail: f32,
    pub end: f32,
}

/// What an entity plays.
#[derive(Debug, Clone, PartialEq)]
pub struct Sequence {
    pub label: String,
    /// The activity it stands for (`ACT_VM_IDLE`), or nothing.
    pub activity: String,
    /// How likely it is chosen among the sequences of its activity.
    pub activity_weight: i32,
    pub flags: i32,
    pub bounds: ([f32; 3], [f32; 3]),
    /// The animations it blends: `blend[0]` of them along the first pose parameter for each of
    /// `blend[1]` along the second, the first parameter's running fastest.
    pub animations: Vec<usize>,
    pub blend: [usize; 2],
    /// The pose parameters across the grid, and the range of each the grid spans.
    pub parameters: [Option<usize>; 2],
    pub parameter_start: [f32; 2],
    pub parameter_end: [f32; 2],
    /// Seconds to blend in from the sequence before it, and out to the next.
    pub fade_in: f32,
    pub fade_out: f32,
    pub events: Vec<Event>,
    pub layers: Vec<AutoLayer>,
    /// How much each bone follows it; empty for all fully.
    pub weights: Vec<f32>,
    pub key_values: String,
    /// The pose parameter that is its cycle, if one is.
    pub cycle_pose: Option<usize>,
}

impl Sequence {
    pub fn looping(&self) -> bool {
        self.flags & flags::LOOPING != 0
    }

    pub fn delta(&self) -> bool {
        self.flags & flags::DELTA != 0
    }

    /// How much bone `bone` follows the sequence.
    pub fn weight(&self, bone: usize) -> f32 {
        if self.weights.is_empty() { 1.0 } else { self.weights.get(bone).copied().unwrap_or(0.0) }
    }
}

/// A stretch of an animation where the model moves along the ground.
#[derive(Debug, Clone, PartialEq)]
pub struct Movement {
    pub end_frame: i32,
    pub flags: i32,
    /// Speed at its start and end.
    pub start_speed: f32,
    pub end_speed: f32,
    pub yaw: f32,
    pub direction: [f32; 3],
    /// Where the model is at its end, from where the animation started.
    pub position: [f32; 3],
}

/// Where the records of some frames are: the `.mdl` (block 0) or a block of the `.ani`, at a
/// byte of that file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frames {
    pub block: usize,
    pub at: usize,
}

/// Bone motion over frames.
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    pub name: String,
    pub fps: f32,
    pub flags: i32,
    pub frames: usize,
    pub movements: Vec<Movement>,
    /// Frames per section, or 0 when one run of records has every frame.
    pub(crate) section_frames: usize,
    pub(crate) sections: Vec<Frames>,
}

/// A bone's place relative to its parent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Local {
    pub position: [f32; 3],
    /// A quaternion: x, y, z, w.
    pub rotation: [f32; 4],
}

impl Local {
    pub const IDENTITY: Local = Local { position: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0] };
}

/// The rotation of Euler angles in radians: about x, then y, then z, as a quaternion. Bones
/// keep their rest rotation both ways, and the two agree this way.
pub fn euler_quaternion(angles: [f32; 3]) -> [f32; 4] {
    let (sx, cx) = (angles[0] * 0.5).sin_cos();
    let (sy, cy) = (angles[1] * 0.5).sin_cos();
    let (sz, cz) = (angles[2] * 0.5).sin_cos();
    [sx * cy * cz - cx * sy * sz, cx * sy * cz + sx * cy * sz, cx * cy * sz - sx * sy * cz, cx * cy * cz + sx * sy * sz]
}

/// A half float.
fn half(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1f);
    let mantissa = f32::from(bits & 0x3ff);
    match exponent {
        0 => sign * mantissa * 2f32.powi(-24),
        // the engine reads these as the largest numbers, not infinities: a bone does not go
        // to infinity
        31 => sign * 65504.0,
        _ => sign * (1.0 + mantissa / 1024.0) * 2f32.powi(exponent - 15),
    }
}

/// The last part of a unit quaternion, from the other three and its sign.
fn complete(x: f32, y: f32, z: f32, negative: bool) -> [f32; 4] {
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    [x, y, z, if negative { -w } else { w }]
}

/// A quaternion in 48 bits: x and y in 16 each, z in 15, and the sign of w.
fn quaternion48(b: &[u8]) -> [f32; 4] {
    let word = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let (x, y, z) = (word(0), word(2), word(4));
    complete(
        (f32::from(x) - 32768.0) / 32768.0,
        (f32::from(y) - 32768.0) / 32768.0,
        (f32::from(z & 0x7fff) - 16384.0) / 16384.0,
        z & 0x8000 != 0,
    )
}

/// A quaternion in 64 bits: x, y and z in 21 each, and the sign of w.
fn quaternion64(b: &[u8]) -> [f32; 4] {
    let bits = u64::from_le_bytes(b[..8].try_into().unwrap());
    let part = |shift: u32| ((bits >> shift) & 0x1f_ffff) as f32;
    let unit = |v: f32| ((v as f64 - 1_048_576.0) / 1_048_576.5) as f32;
    complete(unit(part(0)), unit(part(21)), unit(part(42)), bits >> 63 != 0)
}

/// Three half floats.
fn vector48(b: &[u8]) -> [f32; 3] {
    std::array::from_fn(|i| half(u16::from_le_bytes([b[i * 2], b[i * 2 + 1]])))
}

/// The value of a run-length stream at a frame.
fn stream_value(r: &Reader, mut at: usize, mut frame: usize) -> io::Result<f32> {
    // a stream covers at most the frames of a section, at least one a run: this bounds the walk
    for _ in 0..=u16::MAX {
        let (kept, total) = (usize::from(r.u8(at)?), usize::from(r.u8(at + 1)?));
        if total == 0 {
            return Err(invalid(format!("{}: a run of an animation covers no frames", r.what)));
        }
        if frame < total {
            return match kept {
                0 => Ok(0.0),
                _ => Ok(f32::from(r.i16(at + 2 + 2 * frame.min(kept - 1))?)),
            };
        }
        frame -= total;
        at += 2 * (kept + 1);
    }
    Err(invalid(format!("{}: an animation's values run on without end", r.what)))
}

/// Three values that change over the frames, kept as three streams; one that is not kept is 0.
fn stream_values(r: &Reader, at: usize, frame: usize) -> io::Result<[f32; 3]> {
    let mut values = [0.0; 3];
    for (i, value) in values.iter_mut().enumerate() {
        let offset = r.i16(at + 2 * i)?;
        if offset != 0 {
            let start = usize::try_from(at as i64 + i64::from(offset))
                .map_err(|_| invalid(format!("{}: an animation's values are before the file", r.what)))?;
            *value = stream_value(r, start, frame)?;
        }
    }
    Ok(values)
}

impl Animation {
    pub fn delta(&self) -> bool {
        self.flags & flags::DELTA != 0
    }

    /// Where the records of a frame are, and the frame's number among theirs. The last frame
    /// of an animation cut into sections has a section of its own, after the others.
    fn locate(&self, frame: usize) -> io::Result<(Frames, usize)> {
        let (section, local) = match self.section_frames {
            0 => (0, frame),
            n if self.frames > n && frame + 1 == self.frames => (self.frames / n + 1, 0),
            n => (frame / n, frame % n),
        };
        let frames = self.sections.get(section).copied();
        frames.map(|f| (f, local)).ok_or_else(|| invalid(format!("animation {:?}: frame {frame} has no section", self.name)))
    }

    /// Frame `frame` of the bones it moves: `set` gets each bone's number and what the frame
    /// has of its rotation and its position. `bones` are the model's, `files` the bytes of the
    /// `.mdl` and of each block of the `.ani` (none for those not there).
    pub(crate) fn frame<'a>(
        &self,
        bones: &[Bone],
        files: &dyn Fn(usize) -> Option<(&'a [u8], std::ops::Range<usize>)>,
        frame: usize,
        mut set: impl FnMut(usize, Option<[f32; 4]>, Option<[f32; 3]>),
    ) -> io::Result<()> {
        let (place, local) = self.locate(frame.min(self.frames.saturating_sub(1)))?;
        let Some((data, range)) = files(place.block) else {
            return Err(invalid(format!("animation {:?}: block {} of the .ani is not there", self.name, place.block)));
        };
        let what = format!("animation {:?}", self.name);
        // the records of a block stay within it
        let end = range.end.min(data.len());
        let r = Reader::new(data.get(..end).unwrap_or(data), &what);
        let mut at = place.at;
        if !range.contains(&at) {
            return Err(invalid(format!("{what}: its frames at {at} are outside {range:?}")));
        }
        for _ in 0..=u8::MAX as usize {
            let (bone, kind, next) = (usize::from(r.u8(at)?), r.u8(at + 1)?, r.i16(at + 2)?);
            // records go to bones by number: one for a bone the model does not have moves
            // nothing (an animation that moves no bone has one for bone 255)
            let Some(rest) = bones.get(bone) else {
                if next <= 0 {
                    return Ok(());
                }
                at += next as usize;
                continue;
            };
            let delta = kind & record::DELTA != 0;
            let mut data_at = at + 4;
            let rotation = if kind & record::RAW_ROTATION_64 != 0 {
                data_at += 8;
                Some(quaternion64(r.bytes(at + 4, 8)?))
            } else if kind & record::RAW_ROTATION != 0 {
                data_at += 6;
                Some(quaternion48(r.bytes(at + 4, 6)?))
            } else if kind & record::ROTATION != 0 {
                data_at += 6;
                let values = stream_values(&r, at + 4, local)?;
                let base = if delta { [0.0; 3] } else { rest.euler };
                Some(euler_quaternion(std::array::from_fn(|i| values[i] * rest.rotation_scale[i] + base[i])))
            } else {
                None
            };
            let position = if kind & record::RAW_POSITION != 0 {
                Some(vector48(r.bytes(data_at, 6)?))
            } else if kind & record::POSITION != 0 {
                let values = stream_values(&r, data_at, local)?;
                let base = if delta { [0.0; 3] } else { rest.position };
                Some(std::array::from_fn(|i| values[i] * rest.position_scale[i] + base[i]))
            } else {
                None
            };
            set(bone, rotation, position);
            if next <= 0 {
                return Ok(());
            }
            at += next as usize;
        }
        Err(invalid(format!("{what}: more bone records than bones")))
    }
}

/// The animation descriptions of a model; `blocks` are the byte ranges of the `.ani`'s blocks.
pub(crate) fn animations(r: &Reader, blocks: &[(usize, usize)]) -> io::Result<Vec<Animation>> {
    let count = r.count(180, "animation")?;
    let table = r.offset(0, 184)?;
    r.table(table, count, ANIMATION_SIZE, "animations")?;
    (0..count)
        .map(|i| {
            let at = table + i * ANIMATION_SIZE;
            let name = r.string(r.offset(at, at + 4)?)?;
            let frames = r.count(at + 16, "animation frame")?;
            let movement_count = r.count(at + 20, "movement")?;
            let movement_at = r.offset(at, at + 24)?;
            r.table(movement_at, movement_count, MOVEMENT_SIZE, "movements")?;
            let movements = (0..movement_count)
                .map(|m| {
                    let at = movement_at + m * MOVEMENT_SIZE;
                    Ok(Movement {
                        end_frame: r.i32(at)?,
                        flags: r.i32(at + 4)?,
                        start_speed: r.f32(at + 8)?,
                        end_speed: r.f32(at + 12)?,
                        yaw: r.f32(at + 16)?,
                        direction: r.vec3(at + 20)?,
                        position: r.vec3(at + 32)?,
                    })
                })
                .collect::<io::Result<Vec<Movement>>>()?;
            // where a block's records are: from the start of its range; the .mdl's: from here
            let place = |block: usize, index: i32| -> io::Result<Frames> {
                let base = match block {
                    0 => at,
                    n => blocks
                        .get(n)
                        .map(|&(start, _)| start)
                        .ok_or_else(|| invalid(format!("{}: animation {name:?} is in block {n} of {}", r.what, blocks.len())))?,
                };
                let at = usize::try_from(base as i64 + i64::from(index))
                    .map_err(|_| invalid(format!("{}: animation {name:?} starts before its file", r.what)))?;
                Ok(Frames { block, at })
            };
            let section_frames = r.count(at + 84, "section frame")?;
            let sections = match frames.checked_div(section_frames) {
                None => vec![place(r.count(at + 52, "animation block")?, r.i32(at + 56)?)?],
                // a section for every `section_frames` frames, and one for the last frame
                Some(whole) => {
                    let count = whole + 2;
                    let first = r.offset(at, at + 80)?;
                    r.table(first, count, 8, "animation sections")?;
                    (0..count)
                        .map(|s| place(r.count(first + s * 8, "animation block")?, r.i32(first + s * 8 + 4)?))
                        .collect::<io::Result<Vec<Frames>>>()?
                }
            };
            Ok(Animation { name, fps: r.f32(at + 8)?, flags: r.i32(at + 12)?, frames, movements, section_frames, sections })
        })
        .collect()
}

/// The pose parameters of a model.
pub(crate) fn pose_parameters(r: &Reader) -> io::Result<Vec<PoseParameter>> {
    let count = r.count(300, "pose parameter")?;
    let table = r.offset(0, 304)?;
    r.table(table, count, POSE_PARAMETER_SIZE, "pose parameters")?;
    (0..count)
        .map(|i| {
            let at = table + i * POSE_PARAMETER_SIZE;
            Ok(PoseParameter {
                name: r.string(r.offset(at, at)?)?,
                flags: r.i32(at + 4)?,
                start: r.f32(at + 8)?,
                end: r.f32(at + 12)?,
                wrap: r.f32(at + 16)?,
            })
        })
        .collect()
}

/// An index into a list, or none when it is negative.
fn index(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

/// The sequences of a model with `bones` bones.
pub(crate) fn sequences(r: &Reader, bones: usize, animations: usize) -> io::Result<Vec<Sequence>> {
    let count = r.count(188, "sequence")?;
    let table = r.offset(0, 192)?;
    r.table(table, count, SEQUENCE_SIZE, "sequences")?;
    (0..count)
        .map(|i| {
            let at = table + i * SEQUENCE_SIZE;
            let label = r.string(r.offset(at, at + 4)?)?;
            let activity = r.string(r.offset(at, at + 8)?)?;
            let blend = [r.count(at + 68, "blend")?.max(1), r.count(at + 72, "blend")?.max(1)];
            let anim_count = blend[0]
                .checked_mul(blend[1])
                .filter(|&n| n <= 64 * 64)
                .ok_or_else(|| invalid(format!("{}: sequence {label:?} blends {} by {} animations", r.what, blend[0], blend[1])))?;
            let anim_at = r.offset(at, at + 60)?;
            r.table(anim_at, anim_count, 2, "sequence animations")?;
            let anims = (0..anim_count)
                .map(|k| {
                    let anim = r.i16(anim_at + k * 2)?;
                    usize::try_from(anim)
                        .ok()
                        .filter(|&a| a < animations)
                        .ok_or_else(|| invalid(format!("{}: sequence {label:?} plays animation {anim} of {animations}", r.what)))
                })
                .collect::<io::Result<Vec<usize>>>()?;

            let event_count = r.count(at + 24, "event")?;
            let event_at = r.offset(at, at + 28)?;
            r.table(event_at, event_count, EVENT_SIZE, "events")?;
            let events = (0..event_count)
                .map(|e| {
                    let at = event_at + e * EVENT_SIZE;
                    let options = r.bytes(at + 12, 64)?;
                    let end = options.iter().position(|&b| b == 0).unwrap_or(64);
                    Ok(Event {
                        cycle: r.f32(at)?,
                        event: r.i32(at + 4)?,
                        kind: r.i32(at + 8)?,
                        options: String::from_utf8_lossy(&options[..end]).into_owned(),
                        name: match r.i32(at + 76)? {
                            0 => String::new(),
                            _ => r.string(r.offset(at, at + 76)?)?,
                        },
                    })
                })
                .collect::<io::Result<Vec<Event>>>()?;

            let layer_count = r.count(at + 148, "layer")?;
            let layer_at = r.offset(at, at + 152)?;
            r.table(layer_at, layer_count, LAYER_SIZE, "layers")?;
            let layers = (0..layer_count)
                .map(|l| {
                    let at = layer_at + l * LAYER_SIZE;
                    let flags = r.i32(at + 4)?;
                    let sequence = index(i32::from(r.i16(at)?));
                    let pose = index(i32::from(r.i16(at + 2)?)).filter(|_| flags & layer_flags::POSE != 0);
                    Ok(AutoLayer {
                        sequence: sequence.ok_or_else(|| invalid(format!("{}: a layer of {label:?} plays no sequence", r.what)))?,
                        pose,
                        flags,
                        start: r.f32(at + 8)?,
                        peak: r.f32(at + 12)?,
                        tail: r.f32(at + 16)?,
                        end: r.f32(at + 20)?,
                    })
                })
                .collect::<io::Result<Vec<AutoLayer>>>()?;

            let weights = match r.i32(at + 156)? {
                0 => Vec::new(),
                _ => {
                    let weight_at = r.offset(at, at + 156)?;
                    r.table(weight_at, bones, 4, "bone weights")?;
                    (0..bones).map(|b| r.f32(weight_at + b * 4)).collect::<io::Result<Vec<f32>>>()?
                }
            };
            let key_values = match (r.i32(at + 172)?, r.count(at + 176, "key value byte")?) {
                (0, _) | (_, 0) => String::new(),
                (_, size) => {
                    let bytes = r.bytes(r.offset(at, at + 172)?, size)?;
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                }
            };
            let kind = r.i32(at + 12)?;
            Ok(Sequence {
                label,
                activity,
                activity_weight: r.i32(at + 20)?,
                flags: kind,
                bounds: (r.vec3(at + 32)?, r.vec3(at + 44)?),
                animations: anims,
                blend,
                parameters: [index(r.i32(at + 76)?), index(r.i32(at + 80)?)],
                parameter_start: [r.f32(at + 84)?, r.f32(at + 88)?],
                parameter_end: [r.f32(at + 92)?, r.f32(at + 96)?],
                fade_in: r.f32(at + 104)?,
                fade_out: r.f32(at + 108)?,
                events,
                layers,
                weights,
                key_values,
                cycle_pose: index(r.i32(at + 180)?).filter(|_| kind & flags::CYCLE_POSE != 0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats() {
        assert_eq!(half(0x3c00), 1.0);
        assert_eq!(half(0xc000), -2.0);
        assert_eq!(half(0x3555), 0.333_251_95);
        assert_eq!(half(0x0001), 2f32.powi(-24));
        assert_eq!(half(0x7c00), 65504.0);
    }

    #[test]
    fn packed_quaternions() {
        // the middle of each range is 0: what is left is w, its sign from the top bit
        let mut q48 = [0u8; 6];
        q48[0..2].copy_from_slice(&32768u16.to_le_bytes());
        q48[2..4].copy_from_slice(&32768u16.to_le_bytes());
        q48[4..6].copy_from_slice(&(16384u16 | 0x8000).to_le_bytes());
        assert_eq!(quaternion48(&q48), [0.0, 0.0, 0.0, -1.0]);
        // x at its top: half a turn about x
        q48[0..2].copy_from_slice(&65535u16.to_le_bytes());
        let q = quaternion48(&q48);
        assert!((q[0] - 1.0).abs() < 1e-4 && q[3].abs() < 0.02, "{q:?}");

        let middle = 1_048_576u64;
        let bits = middle | middle << 21 | (middle + 741_455) << 42;
        let q = quaternion64(&bits.to_le_bytes());
        // z = 741455 / 1048576.5: about 0.7071, and w the same
        assert!((q[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5 && (q[3] - q[2]).abs() < 1e-4, "{q:?}");
    }

    #[test]
    fn euler_angles_turn_about_z_then_y_then_x() {
        // a quarter turn about z alone
        let q = euler_quaternion([0.0, 0.0, std::f32::consts::FRAC_PI_2]);
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!(q.iter().zip([0.0, 0.0, half, half]).all(|(a, b)| (a - b).abs() < 1e-6), "{q:?}");
        // all three: the same as x first, then y, then z, each a turn of its own
        let quarter = std::f32::consts::FRAC_PI_2;
        let q = euler_quaternion([quarter, quarter, quarter]);
        assert!(q.iter().zip([0.0, half, 0.0, half]).all(|(a, b)| (a - b).abs() < 1e-6), "{q:?}");
    }

    #[test]
    fn runs_of_values() {
        // a run of 3 frames keeping 2 values, then a run of 2 frames keeping 1
        let mut b = Vec::new();
        b.extend([2u8, 3]);
        for v in [10i16, 20] {
            b.extend(v.to_le_bytes());
        }
        b.extend([1u8, 2]);
        b.extend(30i16.to_le_bytes());
        let r = Reader::new(&b, "test");
        let values: Vec<f32> = (0..5).map(|f| stream_value(&r, 0, f).unwrap()).collect();
        // the last kept value holds for the rest of its run
        assert_eq!(values, [10.0, 20.0, 20.0, 30.0, 30.0]);
        // past the last run: the file ends there
        assert!(stream_value(&r, 0, 5).is_err());
        // a run of no frames would never end
        assert!(stream_value(&Reader::new(&[1, 0, 0, 0], "test"), 0, 1).is_err());
    }
}
