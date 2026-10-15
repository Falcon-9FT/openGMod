//! Where a model's bones are for what it plays.
//!
//! A [`Rig`] is a model put together with the models it includes (`$includemodel`: a player
//! model plays the sequences of `models/m_anm.mdl`) and with the `.ani` file of its animation
//! blocks. The sequences of every model of a rig are its own, matched to its bones by name; a
//! sequence of a model included later does not replace one of the same name, unless it says it
//! overrides it. Pose parameters are shared by name the same way.
//!
//! [`Rig::pose`] works out the bones of a sequence at a point of its cycle: the animations of
//! its grid at the frames the cycle falls between, blended by the pose parameters; the layers
//! the sequence plays on top of itself; then the layers given (gestures). A sequence that is a
//! delta is added to what is under it rather than played instead of it. [`Rig::bone_to_model`]
//! turns a pose into matrices, [`Rig::skinning`] into what moves the vertices.

use std::io;
use std::sync::Arc;

use gmod_fs::Vfs;

use crate::anim::{Animation, AutoLayer, Local, PoseParameter, Sequence, flags, layer_flags};
use crate::mdl::{Attachment, Bone, Mdl};
use crate::reader::invalid;

/// An affine transform: 3 rows of 4, a rotation (and scale) and a translation.
pub type Matrix = [[f32; 4]; 3];

pub const IDENTITY: Matrix = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// `a` after `b`: a point goes through `b` first.
pub fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|row| {
        let r = a[row];
        std::array::from_fn(|column| {
            let along = r[0] * b[0][column] + r[1] * b[1][column] + r[2] * b[2][column];
            if column == 3 { along + r[3] } else { along }
        })
    })
}

/// A point moved by a transform.
pub fn transform(m: &Matrix, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| m[row][0] * p[0] + m[row][1] * p[1] + m[row][2] * p[2] + m[row][3])
}

/// The transform that undoes `m`; the identity when `m` flattens space.
pub fn invert(m: &Matrix) -> Matrix {
    let a = |r: usize, c: usize| m[r][c];
    let cofactor = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)
    };
    let determinant = a(0, 0) * cofactor(0, 0) + a(0, 1) * cofactor(0, 1) + a(0, 2) * cofactor(0, 2);
    if determinant.abs() < 1e-12 {
        return IDENTITY;
    }
    // the inverse of the rotation part is its cofactors turned over, over the determinant
    let mut out = IDENTITY;
    for (r, row) in out.iter_mut().enumerate() {
        for (c, value) in row.iter_mut().take(3).enumerate() {
            *value = cofactor(c, r) / determinant;
        }
    }
    let t = [m[0][3], m[1][3], m[2][3]];
    for row in &mut out {
        row[3] = -(row[0] * t[0] + row[1] * t[1] + row[2] * t[2]);
    }
    out
}

/// The transform of a bone's place relative to its parent.
pub fn local_matrix(local: &Local) -> Matrix {
    let [x, y, z, w] = local.rotation;
    let p = local.position;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w), p[0]],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w), p[1]],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y), p[2]],
    ]
}

/// The rotation of a transform as a quaternion (x, y, z, w), its scale taken out.
pub fn matrix_quaternion(m: &Matrix) -> [f32; 4] {
    let column = |c: usize| {
        let v = [m[0][c], m[1][c], m[2][c]];
        let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
        v.map(|x| x / length)
    };
    let (x, y, z) = (column(0), column(1), column(2));
    let trace = x[0] + y[1] + z[2];
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(y[2] - z[1]) / s, (z[0] - x[2]) / s, (x[1] - y[0]) / s, s / 4.0]
    } else if x[0] > y[1] && x[0] > z[2] {
        let s = (1.0 + x[0] - y[1] - z[2]).sqrt() * 2.0;
        [s / 4.0, (y[0] + x[1]) / s, (z[0] + x[2]) / s, (y[2] - z[1]) / s]
    } else if y[1] > z[2] {
        let s = (1.0 + y[1] - x[0] - z[2]).sqrt() * 2.0;
        [(y[0] + x[1]) / s, s / 4.0, (z[1] + y[2]) / s, (z[0] - x[2]) / s]
    } else {
        let s = (1.0 + z[2] - x[0] - y[1]).sqrt() * 2.0;
        [(z[0] + x[2]) / s, (z[1] + y[2]) / s, s / 4.0, (x[1] - y[0]) / s]
    };
    normalized(q)
}

fn normalized(q: [f32; 4]) -> [f32; 4] {
    let length = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if length > 1e-12 { q.map(|v| v / length) } else { [0.0, 0.0, 0.0, 1.0] }
}

/// `a` after `b`, as rotations.
pub fn quaternion_multiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

/// From `a` (t = 0) to `b` (t = 1) the short way round, at an even pace.
pub fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut cos = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let b = if cos < 0.0 {
        cos = -cos;
        b.map(|v| -v)
    } else {
        b
    };
    let (wa, wb) = if cos > 0.9995 {
        // close enough to go straight
        (1.0 - t, t)
    } else {
        let angle = cos.acos();
        let sin = angle.sin();
        (((1.0 - t) * angle).sin() / sin, (t * angle).sin() / sin)
    };
    normalized(std::array::from_fn(|i| a[i] * wa + b[i] * wb))
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// A sequence played over another (a gesture): a sequence of the rig at a point of its cycle,
/// weighing that much.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    pub sequence: usize,
    pub cycle: f32,
    pub weight: f32,
}

/// One of the models of a rig, and what ties it to the rig.
#[derive(Debug, Clone)]
struct Part {
    mdl: Mdl,
    /// The `.ani` file of its animation blocks, when it has one and it is there.
    blocks: Option<Arc<[u8]>>,
    /// The rig's bone for each of its bones, and its bone for each of the rig's.
    to_rig: Vec<Option<usize>>,
    from_rig: Vec<Option<usize>>,
    /// The rig's pose parameter for each of its own.
    poses: Vec<usize>,
}

/// A model with the models it includes, ready to be posed.
#[derive(Debug, Clone)]
pub struct Rig {
    parts: Vec<Part>,
    /// Every sequence the rig plays: its part and its number there.
    sequences: Vec<(usize, usize)>,
    pose_parameters: Vec<PoseParameter>,
    attachments: Vec<Attachment>,
}

/// How deep sequences may play layers of layers: a model whose layers play each other stops
/// there.
const MAX_LAYER_DEPTH: u32 = 4;

impl Rig {
    /// A model on its own, with the `.ani` file of its animation blocks.
    pub fn new(model: Mdl, blocks: Option<Vec<u8>>) -> Rig {
        let bones = model.bones.len();
        let identity: Vec<Option<usize>> = (0..bones).map(Some).collect();
        let mut rig = Rig {
            sequences: (0..model.sequences.len()).map(|s| (0, s)).collect(),
            pose_parameters: model.pose_parameters.clone(),
            attachments: model.attachments.clone(),
            parts: Vec::new(),
        };
        let poses = (0..model.pose_parameters.len()).collect();
        rig.parts.push(Part { mdl: model, blocks: blocks.map(Arc::from), to_rig: identity.clone(), from_rig: identity, poses });
        rig
    }

    /// Adds a model the rig's model includes: its sequences, pose parameters and attachments,
    /// on the rig's bones of the same names.
    pub fn include(&mut self, model: Mdl, blocks: Option<Vec<u8>>) {
        let bones = self.bones();
        let find = |name: &str| bones.iter().position(|b| b.name.eq_ignore_ascii_case(name));
        let to_rig: Vec<Option<usize>> = model.bones.iter().map(|b| find(&b.name)).collect();
        let mut from_rig = vec![None; bones.len()];
        for (own, rig) in to_rig.iter().enumerate() {
            if let Some(rig) = *rig {
                from_rig[rig].get_or_insert(own);
            }
        }
        let poses = model
            .pose_parameters
            .iter()
            .map(|p| match self.pose_parameters.iter().position(|q| q.name.eq_ignore_ascii_case(&p.name)) {
                Some(i) => i,
                None => {
                    self.pose_parameters.push(p.clone());
                    self.pose_parameters.len() - 1
                }
            })
            .collect();
        for attachment in &model.attachments {
            let known = self.attachments.iter().any(|a| a.name.eq_ignore_ascii_case(&attachment.name));
            if let (false, Some(Some(bone))) = (known, to_rig.get(attachment.bone)) {
                self.attachments.push(Attachment { bone: *bone, ..attachment.clone() });
            }
        }
        let part = self.parts.len();
        for (s, sequence) in model.sequences.iter().enumerate() {
            // the model's own sequences already in the rig are not in its parts yet
            let label = |p: usize, t: usize| if p == part { &model.sequences[t].label } else { &self.parts[p].mdl.sequences[t].label };
            let same = self.sequences.iter().position(|&(p, t)| label(p, t).eq_ignore_ascii_case(&sequence.label));
            match same {
                None => self.sequences.push((part, s)),
                Some(i) if sequence.flags & flags::OVERRIDE != 0 => self.sequences[i] = (part, s),
                Some(_) => {}
            }
        }
        self.parts.push(Part { mdl: model, blocks: blocks.map(Arc::from), to_rig, from_rig, poses });
    }

    /// Reads a model (`models/player/kleiner.mdl`) from the game's files with the `.ani` file
    /// of its blocks and the models it includes, and those they include, each once. An
    /// included model that is not there is left out, as the game leaves it; so is an `.ani`
    /// that is not there or is not one: the animations in its blocks then do not move
    /// ([`Rig::read_all`] says so).
    pub fn load(vfs: &Vfs, path: &str) -> io::Result<Rig> {
        let read = |path: &str| -> io::Result<(Mdl, Option<Vec<u8>>)> {
            let mdl = Mdl::parse(&vfs.read(path, "GAME")?).map_err(|e| crate::context(path, e))?;
            let blocks = mdl
                .animation_file
                .as_ref()
                .and_then(|name| vfs.read(&name.replace('\\', "/"), "GAME").ok())
                .filter(|bytes| bytes.starts_with(b"IDAG"));
            Ok((mdl, blocks))
        };
        let normal = |path: &str| path.replace('\\', "/").to_ascii_lowercase();
        let (mdl, blocks) = read(path)?;
        let mut seen = vec![normal(path)];
        let mut waiting: Vec<String> = mdl.include_models.iter().rev().cloned().collect();
        let mut rig = Rig::new(mdl, blocks);
        // depth first, in order: an included model's own includes come right after it
        while let Some(include) = waiting.pop() {
            let name = normal(&include);
            if seen.contains(&name) || seen.len() > 64 {
                continue;
            }
            seen.push(name.clone());
            if let Ok((mdl, blocks)) = read(&name) {
                waiting.extend(mdl.include_models.iter().rev().cloned());
                rig.include(mdl, blocks);
            }
        }
        Ok(rig)
    }

    /// The model itself.
    pub fn mdl(&self) -> &Mdl {
        &self.parts[0].mdl
    }

    pub fn bones(&self) -> &[Bone] {
        &self.parts[0].mdl.bones
    }

    pub fn sequence_count(&self) -> usize {
        self.sequences.len()
    }

    pub fn sequence(&self, sequence: usize) -> Option<&Sequence> {
        self.sequences.get(sequence).map(|&(p, s)| &self.parts[p].mdl.sequences[s])
    }

    /// The sequence of a label, ignoring case.
    pub fn find_sequence(&self, label: &str) -> Option<usize> {
        (0..self.sequences.len()).find(|&i| self.sequence(i).is_some_and(|s| s.label.eq_ignore_ascii_case(label)))
    }

    pub fn pose_parameters(&self) -> &[PoseParameter] {
        &self.pose_parameters
    }

    /// The pose parameters as a model starts: each at the value 0 of its units, from 0 to 1
    /// across its range.
    pub fn default_pose_parameters(&self) -> Vec<f32> {
        self.pose_parameters.iter().map(|p| p.normalize(0.0)).collect()
    }

    /// The attachments of the model and of those it includes, on the rig's bones.
    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    /// The animations of a sequence's grid that count for these pose parameters (from 0 to 1,
    /// one for each of the rig's), each with its weight; and the sequence's part.
    fn grid(&self, part: usize, sequence: &Sequence, parameters: &[f32]) -> Vec<(usize, f32)> {
        let place = |axis: usize| -> (usize, f32) {
            let count = sequence.blend[axis];
            if count <= 1 {
                return (0, 0.0);
            }
            let along = sequence.parameters[axis].and_then(|p| self.parts[part].poses.get(p)).map_or(0.0, |&rig| {
                let pose = &self.pose_parameters[rig];
                let mut value = pose.value(parameters.get(rig).copied().unwrap_or_else(|| pose.normalize(0.0)));
                if pose.wrap != 0.0 {
                    // the same angle within a turn around the middle of the range
                    let middle = (pose.start + pose.end) / 2.0;
                    value -= pose.wrap * ((value - middle) / pose.wrap + 0.5).floor();
                }
                let (start, end) = (sequence.parameter_start[axis], sequence.parameter_end[axis]);
                if end == start { 0.0 } else { ((value - start) / (end - start)).clamp(0.0, 1.0) }
            });
            let position = along * (count - 1) as f32;
            let index = (position.floor() as usize).min(count - 2);
            (index, position - index as f32)
        };
        let ((x, fx), (y, fy)) = (place(0), place(1));
        let columns = sequence.blend[0];
        let mut cells = vec![(x + y * columns, (1.0 - fx) * (1.0 - fy))];
        if sequence.blend[0] > 1 {
            cells.push((x + 1 + y * columns, fx * (1.0 - fy)));
        }
        if sequence.blend[1] > 1 {
            cells.push((x + (y + 1) * columns, (1.0 - fx) * fy));
            if sequence.blend[0] > 1 {
                cells.push((x + 1 + (y + 1) * columns, fx * fy));
            }
        }
        cells.into_iter().filter_map(|(cell, weight)| Some((*sequence.animations.get(cell)?, weight))).collect()
    }

    /// How many cycles of a sequence play in a second, for these pose parameters: its
    /// animations' rates over their frames, blended.
    pub fn cycle_rate(&self, sequence: usize, parameters: &[f32]) -> f32 {
        let Some(&(part, s)) = self.sequences.get(sequence) else { return 0.0 };
        let mdl = &self.parts[part].mdl;
        self.grid(part, &mdl.sequences[s], parameters)
            .into_iter()
            .map(|(a, weight)| {
                let animation = &mdl.animations[a];
                if animation.frames > 1 { weight * animation.fps / (animation.frames - 1) as f32 } else { 0.0 }
            })
            .sum()
    }

    /// How long a cycle of a sequence takes, in seconds; 0 for one that does not move.
    pub fn duration(&self, sequence: usize, parameters: &[f32]) -> f32 {
        let rate = self.cycle_rate(sequence, parameters);
        if rate > 0.0 { 1.0 / rate } else { 0.0 }
    }

    /// How far a model moves over a cycle of a sequence (its animations' movement, blended).
    pub fn movement(&self, sequence: usize, parameters: &[f32]) -> [f32; 3] {
        let Some(&(part, s)) = self.sequences.get(sequence) else { return [0.0; 3] };
        let mdl = &self.parts[part].mdl;
        self.grid(part, &mdl.sequences[s], parameters).into_iter().fold([0.0; 3], |sum, (a, weight)| {
            let end = mdl.animations[a].movements.last().map_or([0.0; 3], |m| m.position);
            std::array::from_fn(|i| sum[i] + end[i] * weight)
        })
    }

    /// How fast a model moves along the ground playing a sequence, in units a second.
    pub fn ground_speed(&self, sequence: usize, parameters: &[f32]) -> f32 {
        let moved = self.movement(sequence, parameters);
        (moved[0] * moved[0] + moved[1] * moved[1] + moved[2] * moved[2]).sqrt() * self.cycle_rate(sequence, parameters)
    }

    /// The rest pose: every bone where the model was made with it.
    pub fn rest(&self) -> Vec<Local> {
        self.bones().iter().map(|b| Local { position: b.position, rotation: b.rotation }).collect()
    }

    /// Where the bones are playing a sequence at a point of its cycle (0 to 1), with pose
    /// parameters (0 to 1, one for each of the rig's; missing ones at their start) and layers
    /// on top. What cannot be read of an animation leaves the bones it moves where they are.
    pub fn pose(&self, sequence: usize, cycle: f32, parameters: &[f32], layers: &[Layer]) -> Vec<Local> {
        let mut pose = self.rest();
        if let Some(&(part, s)) = self.sequences.get(sequence) {
            self.accumulate(&mut pose, part, s, cycle, 1.0, parameters, 0);
        }
        for layer in layers {
            if let Some(&(part, s)) = self.sequences.get(layer.sequence) {
                self.accumulate(&mut pose, part, s, layer.cycle, layer.weight, parameters, 0);
            }
        }
        // the sequences played always, over all of it, at their start: those are poses held
        // by pose parameters
        for &(part, s) in &self.sequences {
            if self.parts[part].mdl.sequences.get(s).is_some_and(|seq| seq.flags & flags::AUTOPLAY != 0) {
                self.accumulate(&mut pose, part, s, 0.0, 1.0, parameters, 0);
            }
        }
        pose
    }

    /// Plays a sequence of a part over `pose` with a weight, and the layers it plays on top of
    /// itself.
    #[allow(clippy::too_many_arguments)]
    fn accumulate(&self, pose: &mut [Local], part: usize, sequence: usize, cycle: f32, weight: f32, parameters: &[f32], depth: u32) {
        if weight <= 0.0 || depth > MAX_LAYER_DEPTH {
            return;
        }
        let Some(seq) = self.parts[part].mdl.sequences.get(sequence) else { return };
        let mut own = self.single(part, seq, cycle, parameters);
        for layer in seq.layers.iter().filter(|l| l.flags & layer_flags::LOCAL != 0) {
            if let Some((cycle, weight)) = self.layer_at(part, layer, cycle, 1.0, parameters) {
                self.accumulate(&mut own, part, layer.sequence, cycle, weight, parameters, depth + 1);
            }
        }
        let from_rig = &self.parts[part].from_rig;
        for (bone, (into, from)) in pose.iter_mut().zip(&own).enumerate() {
            // a bone the sequence's model does not have, it does not move
            let Some(own_bone) = from_rig[bone] else { continue };
            let w = weight * seq.weight(own_bone);
            if w <= 0.0 {
                continue;
            }
            if seq.delta() {
                let turn = slerp([0.0, 0.0, 0.0, 1.0], from.rotation, w);
                into.rotation = if seq.flags & flags::POST != 0 {
                    quaternion_multiply(into.rotation, turn)
                } else {
                    quaternion_multiply(turn, into.rotation)
                };
                into.position = std::array::from_fn(|i| into.position[i] + from.position[i] * w);
            } else {
                let w = w.min(1.0);
                into.rotation = slerp(into.rotation, from.rotation, w);
                into.position = lerp(into.position, from.position, w);
            }
        }
        for layer in seq.layers.iter().filter(|l| l.flags & layer_flags::LOCAL == 0) {
            if let Some((cycle, weight)) = self.layer_at(part, layer, cycle, weight, parameters) {
                self.accumulate(pose, part, layer.sequence, cycle, weight, parameters, depth + 1);
            }
        }
    }

    /// A layer's cycle and weight while its sequence is at `cycle` with `weight`: it fades in
    /// from its start to its peak and out from its tail to its end, over the cycle or over its
    /// pose parameter's value; none outside.
    fn layer_at(&self, part: usize, layer: &AutoLayer, cycle: f32, weight: f32, parameters: &[f32]) -> Option<(f32, f32)> {
        if layer.start == layer.end {
            return Some((cycle, weight));
        }
        let by_pose = layer.flags & layer_flags::POSE != 0;
        let at = if by_pose {
            layer.pose.and_then(|p| self.parts[part].poses.get(p)).map_or(0.0, |&rig| {
                let pose = &self.pose_parameters[rig];
                pose.value(parameters.get(rig).copied().unwrap_or_else(|| pose.normalize(0.0)))
            })
        } else {
            cycle
        };
        if at < layer.start || at >= layer.end {
            return None;
        }
        let mut s = if at < layer.peak && layer.start != layer.peak {
            (at - layer.start) / (layer.peak - layer.start)
        } else if at > layer.tail && layer.end != layer.tail {
            (layer.end - at) / (layer.end - layer.tail)
        } else {
            1.0
        };
        if layer.flags & layer_flags::SPLINE != 0 {
            s = s * s * (3.0 - 2.0 * s);
        }
        let weight = if layer.flags & layer_flags::CROSS_FADE != 0 && at > layer.tail {
            s * weight / (1.0 - weight + s * weight)
        } else if layer.flags & layer_flags::NO_BLEND != 0 {
            s
        } else {
            weight * s
        };
        let cycle = if by_pose { cycle } else { (cycle - layer.start) / (layer.end - layer.start) };
        Some((cycle, weight))
    }

    /// A sequence's own pose at a point of its cycle: its animations, blended. Bones its model
    /// does not have stay at the rig's rest.
    fn single(&self, part: usize, sequence: &Sequence, cycle: f32, parameters: &[f32]) -> Vec<Local> {
        let mut pose = if sequence.delta() { vec![Local::IDENTITY; self.bones().len()] } else { self.rest() };
        let mut so_far = 0.0;
        let mut animated = pose.clone();
        for (animation, weight) in self.grid(part, sequence, parameters) {
            if weight <= 0.0 {
                continue;
            }
            animated.clone_from(&pose);
            if self.animation_pose(part, animation, cycle, &mut animated).is_err() {
                continue;
            }
            so_far += weight;
            let t = weight / so_far;
            for (into, from) in pose.iter_mut().zip(&animated) {
                into.rotation = slerp(into.rotation, from.rotation, t);
                into.position = lerp(into.position, from.position, t);
            }
        }
        pose
    }

    /// An animation of a part at a point of its cycle, between its two nearest frames, into
    /// the rig's bones of `pose`.
    fn animation_pose(&self, part: usize, animation: usize, cycle: f32, pose: &mut [Local]) -> io::Result<()> {
        let p = &self.parts[part];
        let Some(anim) = p.mdl.animations.get(animation) else { return Err(invalid("no such animation")) };
        let frame = cycle.clamp(0.0, 1.0) * anim.frames.saturating_sub(1) as f32;
        let first = frame.floor() as usize;
        let t = frame - first as f32;
        self.frame(p, anim, first, pose)?;
        if t > 1e-4 && first + 1 < anim.frames {
            let mut next = pose.to_vec();
            self.frame(p, anim, first + 1, &mut next)?;
            for (into, to) in pose.iter_mut().zip(&next) {
                into.rotation = slerp(into.rotation, to.rotation, t);
                into.position = lerp(into.position, to.position, t);
            }
        }
        Ok(())
    }

    /// A frame of an animation of a part into the rig's bones of `pose`: the part's bones it
    /// does not move at the part's rest (none at all for a delta).
    fn frame(&self, part: &Part, animation: &Animation, frame: usize, pose: &mut [Local]) -> io::Result<()> {
        let delta = animation.delta();
        for (own, rig) in part.to_rig.iter().enumerate() {
            if let Some(slot) = rig.and_then(|r| pose.get_mut(r)) {
                let bone = &part.mdl.bones[own];
                *slot = if delta { Local::IDENTITY } else { Local { position: bone.position, rotation: bone.rotation } };
            }
        }
        let files = |block: usize| -> Option<(&[u8], std::ops::Range<usize>)> {
            match block {
                0 => Some((&part.mdl.data.0[..], 0..part.mdl.data.0.len())),
                n => {
                    let blocks = part.blocks.as_deref()?;
                    let &(start, end) = part.mdl.animation_blocks.get(n)?;
                    Some((blocks, start..end))
                }
            }
        };
        animation.frame(&part.mdl.bones, &files, frame, |bone, rotation, position| {
            if let Some(slot) = part.to_rig.get(bone).copied().flatten().and_then(|r| pose.get_mut(r)) {
                if let Some(rotation) = rotation {
                    slot.rotation = rotation;
                }
                if let Some(position) = position {
                    slot.position = position;
                }
            }
        })
    }

    /// Reads every frame of every animation of the rig's models: how many frames there were,
    /// or the first that could not be read.
    pub fn read_all(&self) -> io::Result<usize> {
        self.read(&self.parts)
    }

    /// The same for the model's own animations, not those of the models it includes.
    pub fn read_own(&self) -> io::Result<usize> {
        self.read(&self.parts[..1])
    }

    fn read(&self, parts: &[Part]) -> io::Result<usize> {
        let mut frames = 0;
        for part in parts {
            let mut pose = self.rest();
            for animation in &part.mdl.animations {
                for frame in 0..animation.frames {
                    self.frame(part, animation, frame, &mut pose).map_err(|e| crate::context(&part.mdl.name, e))?;
                }
                frames += animation.frames;
            }
        }
        Ok(frames)
    }

    /// Where each bone of a pose is in the model's space.
    pub fn bone_to_model(&self, pose: &[Local]) -> Vec<Matrix> {
        let mut out: Vec<Matrix> = Vec::with_capacity(pose.len());
        for (bone, local) in self.bones().iter().zip(pose) {
            let own = local_matrix(local);
            // parents come before their children; one that does not is taken for a root
            let parent = usize::try_from(bone.parent).ok().filter(|&p| p < out.len());
            out.push(match parent {
                Some(p) => multiply(&out[p], &own),
                None => own,
            });
        }
        out
    }

    /// What moves the vertices: from where the model was made with them to where `bones`
    /// (bone to model) has the bones.
    pub fn skinning(&self, bones: &[Matrix]) -> Vec<Matrix> {
        self.bones().iter().zip(bones).map(|(bone, m)| multiply(m, &bone.pose_to_bone)).collect()
    }

    /// Where an attachment is in the model's space, for bones from [`Rig::bone_to_model`].
    pub fn attachment(&self, attachment: usize, bones: &[Matrix]) -> Option<Matrix> {
        let a = self.attachments.get(attachment)?;
        Some(multiply(bones.get(a.bone)?, &a.local))
    }
}
