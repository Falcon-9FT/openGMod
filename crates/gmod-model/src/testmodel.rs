//! Small model files written for tests, also those of crates that read models (feature
//! `testmodel`): a model header, physics models of convex pieces, and models with bones,
//! animations and sequences ([`TestRig`]).

use crate::anim::{AutoLayer, PoseParameter, euler_quaternion};
use crate::phy::to_ivp;
use crate::pose::{Matrix, invert, local_matrix, multiply};

/// A model header (`.mdl`) without bones, textures or meshes: its checksum and its hull.
pub fn mdl(checksum: i32, hull: ([f32; 3], [f32; 3])) -> Vec<u8> {
    let mut b = vec![0u8; 408];
    b[0..4].copy_from_slice(b"IDST");
    b[4..8].copy_from_slice(&48i32.to_le_bytes());
    b[8..12].copy_from_slice(&checksum.to_le_bytes());
    for (i, v) in hull.0.iter().chain(&hull.1).enumerate() {
        b[104 + i * 4..108 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    b
}

/// A convex piece for [`phy`]: points in the model's units, and triangles in any winding.
#[derive(Debug, Clone, PartialEq)]
pub struct TestConvex {
    pub points: Vec<[f32; 3]>,
    pub triangles: Vec<[u16; 3]>,
}

impl TestConvex {
    /// A box: corner `i` has the maximum along x, y, z where bit 0, 1, 2 of `i` is set.
    pub fn cuboid(mins: [f32; 3], maxs: [f32; 3]) -> TestConvex {
        let points = (0..8).map(|i| std::array::from_fn(|axis| if i & (1 << axis) == 0 { mins[axis] } else { maxs[axis] })).collect();
        let triangles = vec![
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ];
        TestConvex { points, triangles }
    }
}

fn put(out: &mut Vec<u8>, values: &[i32]) {
    for v in values {
        out.extend(v.to_le_bytes());
    }
}

/// A compact surface of convex pieces. Its points are one pool for all ledges; the inner nodes
/// of its tree point to a hull of everything, a box around it, which is no part of the shape.
fn compact_surface(convexes: &[TestConvex]) -> Vec<u8> {
    let mut pool: Vec<[f32; 3]> = Vec::new();
    let mut ledges: Vec<(usize, &[[u16; 3]], bool)> = Vec::new();
    for convex in convexes {
        ledges.push((pool.len(), &convex.triangles, false));
        pool.extend(&convex.points);
    }
    let (mut lo, mut hi) = ([0.0f32; 3], [0.0f32; 3]);
    if let Some(first) = pool.first() {
        (lo, hi) = (*first, *first);
    }
    for p in &pool {
        lo = std::array::from_fn(|i| lo[i].min(p[i]));
        hi = std::array::from_fn(|i| hi[i].max(p[i]));
    }
    let hull = TestConvex::cuboid(lo.map(|v| v - 1.0), hi.map(|v| v + 1.0));
    let hull_ledge = ledges.len();
    ledges.push((pool.len(), &hull.triangles, true));
    pool.extend(&hull.points);

    let mut ledge_at = Vec::new();
    let mut at = 48;
    for (_, triangles, _) in &ledges {
        ledge_at.push(at);
        at += 16 + 16 * triangles.len();
    }
    let pool_at = at;
    let nodes_at = pool_at + 16 * pool.len();

    // the tree, written parents first: (offset of the right child in nodes, ledge)
    fn tree(pieces: std::ops::Range<usize>, hull: usize, nodes: &mut Vec<(usize, usize)>) {
        if pieces.len() <= 1 {
            nodes.push((0, if pieces.is_empty() { hull } else { pieces.start }));
            return;
        }
        let me = nodes.len();
        nodes.push((0, hull));
        let middle = pieces.start + pieces.len() / 2;
        tree(pieces.start..middle, hull, nodes);
        nodes[me].0 = nodes.len() - me;
        tree(middle..pieces.end, hull, nodes);
    }
    let mut nodes = Vec::new();
    tree(0..convexes.len(), hull_ledge, &mut nodes);
    let size = nodes_at + 28 * nodes.len();

    let mut out = Vec::with_capacity(size);
    for v in [0.0f32, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0] {
        out.extend(v.to_le_bytes());
    }
    put(&mut out, &[(size as i32) << 8, nodes_at as i32, 0, 0]);
    out.extend(b"IVPS");
    for ((first, triangles, children), &at) in ledges.iter().zip(&ledge_at) {
        let flags = u32::from(*children) | 1 << 2 | ((16 + 16 * triangles.len()) as u32 / 16) << 8;
        put(&mut out, &[pool_at as i32 - at as i32, 0, flags as i32]);
        out.extend((triangles.len() as i16).to_le_bytes());
        out.extend(0i16.to_le_bytes());
        for (t, tri) in triangles.iter().enumerate() {
            out.extend((t as u32 & 0xFFF).to_le_bytes());
            for corner in tri {
                out.extend((*first as u32 + u32::from(*corner)).to_le_bytes());
            }
        }
    }
    for p in &pool {
        for v in to_ivp(*p) {
            out.extend(v.to_le_bytes());
        }
        out.extend(0.0f32.to_le_bytes());
    }
    for (n, (right, ledge)) in nodes.iter().enumerate() {
        let node_at = nodes_at + 28 * n;
        put(&mut out, &[(right * 28) as i32, ledge_at[*ledge] as i32 - node_at as i32]);
        out.extend([0u8; 20]);
    }
    assert_eq!(out.len(), size);
    out
}

/// A physics model (`.phy`) of `solids`, each made of convex pieces, with `text` after them.
/// The solids have a `"VPHY"` header, or none (`legacy`) as in models from before 2006.
pub fn phy(checksum: i32, solids: &[Vec<TestConvex>], legacy: bool, text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put(&mut out, &[16, 0, solids.len() as i32, checksum]);
    for convexes in solids {
        let surface = compact_surface(convexes);
        let mut data = Vec::new();
        if !legacy {
            data.extend(b"VPHY");
            data.extend(0x100i16.to_le_bytes());
            data.extend(0i16.to_le_bytes());
            put(&mut data, &[surface.len() as i32, 0, 0, 0, 0]);
        }
        data.extend(surface);
        put(&mut out, &[data.len() as i32]);
        out.extend(data);
    }
    out.extend(text.as_bytes());
    out.push(0);
    out
}

/// A bone of a [`TestRig`]: its parent (-1 for none) and its rest place, the rotation as Euler
/// angles in radians.
#[derive(Debug, Clone, PartialEq)]
pub struct TestBone {
    pub name: String,
    pub parent: i32,
    pub position: [f32; 3],
    pub euler: [f32; 3],
}

/// How a [`TestAnimation`] keeps the frames of its bones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// Values that change, in runs: what most animations have.
    Streams,
    /// One rotation in 48 or 64 bits and one position in half floats, the first frame's, for
    /// every frame.
    Raw48,
    Raw64,
}

/// The bones a frame moves: bone, position (none: the rotation only) and Euler angles - for a
/// delta, what is added to the bone's rest.
pub type TestFrame = Vec<(usize, Option<[f32; 3]>, [f32; 3])>;

/// An animation of a [`TestRig`].
#[derive(Debug, Clone, PartialEq)]
pub struct TestAnimation {
    pub name: String,
    pub fps: f32,
    pub flags: i32,
    /// For every frame, the bones it moves.
    pub frames: Vec<TestFrame>,
    pub keep: Keep,
    /// Frames per section; 0 for one run of records.
    pub section_frames: usize,
    /// The block of the `.ani` file it is kept in; 0 for the model itself.
    pub block: usize,
    /// Where the model is at its end, when it moves along the ground.
    pub moves: Option<[f32; 3]>,
}

impl TestAnimation {
    /// Frames of one bone turning about z from `from` to `to` radians, evenly.
    pub fn turning(name: &str, bone: usize, frames: usize, from: f32, to: f32) -> TestAnimation {
        let frames = (0..frames)
            .map(|f| {
                let t = if frames > 1 { f as f32 / (frames - 1) as f32 } else { 0.0 };
                vec![(bone, None, [0.0, 0.0, from + (to - from) * t])]
            })
            .collect();
        TestAnimation { name: name.to_string(), fps: 30.0, flags: 0, frames, keep: Keep::Streams, section_frames: 0, block: 0, moves: None }
    }
}

/// A sequence of a [`TestRig`].
#[derive(Debug, Clone, PartialEq)]
pub struct TestSequence {
    pub label: String,
    pub activity: String,
    pub activity_weight: i32,
    pub flags: i32,
    /// The grid of animations, `blend[0]` along the first parameter for each along the second.
    pub animations: Vec<usize>,
    pub blend: [usize; 2],
    pub parameters: [i32; 2],
    pub parameter_start: [f32; 2],
    pub parameter_end: [f32; 2],
    pub layers: Vec<AutoLayer>,
    /// Per bone; empty for none in the file.
    pub weights: Vec<f32>,
    /// Cycle, event number, options.
    pub events: Vec<(f32, i32, String)>,
}

impl TestSequence {
    /// A sequence playing one animation.
    pub fn new(label: &str, activity: &str, animation: usize) -> TestSequence {
        TestSequence {
            label: label.to_string(),
            activity: activity.to_string(),
            activity_weight: 1,
            flags: 0,
            animations: vec![animation],
            blend: [1, 1],
            parameters: [-1, -1],
            parameter_start: [0.0; 2],
            parameter_end: [0.0; 2],
            layers: Vec::new(),
            weights: Vec::new(),
            events: Vec::new(),
        }
    }
}

/// A model with bones that plays animations: what [`TestRig::files`] writes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TestRig {
    pub name: String,
    pub checksum: i32,
    pub bones: Vec<TestBone>,
    pub animations: Vec<TestAnimation>,
    pub sequences: Vec<TestSequence>,
    pub pose_parameters: Vec<PoseParameter>,
    pub include_models: Vec<String>,
    /// Name, bone and where on it.
    pub attachments: Vec<(String, usize, [f32; 3])>,
    /// The `.ani` file of the blocks the animations name.
    pub animation_file: String,
}

/// What one stored step of a bone's angle and position is.
pub const ROTATION_SCALE: f32 = 1.0 / 4096.0;
pub const POSITION_SCALE: f32 = 1.0 / 64.0;

/// Little-endian bytes with fields filled in later.
#[derive(Default)]
struct Bytes(Vec<u8>);

impl Bytes {
    fn at(&self) -> usize {
        self.0.len()
    }

    fn i32(&mut self, v: i32) -> &mut Bytes {
        self.0.extend(v.to_le_bytes());
        self
    }

    fn i16(&mut self, v: i16) -> &mut Bytes {
        self.0.extend(v.to_le_bytes());
        self
    }

    fn f32s(&mut self, v: &[f32]) -> &mut Bytes {
        for x in v {
            self.0.extend(x.to_le_bytes());
        }
        self
    }

    fn zeros(&mut self, n: usize) -> &mut Bytes {
        self.0.resize(self.0.len() + n, 0);
        self
    }

    fn set(&mut self, at: usize, v: i32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn set16(&mut self, at: usize, v: i16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }

    /// An offset from `base` to here, written at `field`.
    fn point(&mut self, field: usize, base: usize) {
        let here = self.at() as i32;
        self.set(field, here - base as i32);
    }

    fn string(&mut self, s: &str) -> usize {
        let at = self.at();
        self.0.extend(s.as_bytes());
        self.0.push(0);
        at
    }
}

/// A half float, rounded towards zero.
fn to_half(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    match exponent {
        e if e <= 0 => sign,
        e if e >= 31 => sign | 0x7bff,
        e => sign | (e as u16) << 10 | ((bits >> 13) & 0x3ff) as u16,
    }
}

/// The values of a run-length stream: runs of up to 255 frames, keeping values up to the last
/// change of each.
fn stream(values: &[i16]) -> Vec<u8> {
    let mut out = Vec::new();
    for run in values.chunks(255) {
        let mut kept = run.len();
        while kept > 1 && run[kept - 1] == run[kept - 2] {
            kept -= 1;
        }
        out.push(kept as u8);
        out.push(run.len() as u8);
        for v in &run[..kept] {
            out.extend(v.to_le_bytes());
        }
    }
    out
}

impl TestRig {
    /// Where each bone is at rest, in the model's space.
    pub fn rest_bone_to_model(&self) -> Vec<Matrix> {
        let mut out: Vec<Matrix> = Vec::new();
        for bone in &self.bones {
            let local = local_matrix(&crate::anim::Local { position: bone.position, rotation: euler_quaternion(bone.euler) });
            let world = match usize::try_from(bone.parent).ok().and_then(|p| out.get(p)) {
                Some(parent) => multiply(parent, &local),
                None => local,
            };
            out.push(world);
        }
        out
    }

    /// The records of one bone over frames `frames` of an animation.
    fn record(&self, animation: &TestAnimation, bone: usize, frames: &[usize]) -> Vec<u8> {
        let delta = animation.flags & crate::anim::flags::DELTA != 0;
        let rest = &self.bones[bone];
        let values = |f: usize| animation.frames[f].iter().find(|(b, ..)| *b == bone).map(|&(_, p, e)| (p, e)).unwrap_or_default();
        let moves = values(frames[0]).0.is_some();
        let mut out = vec![bone as u8, 0, 0, 0];
        match animation.keep {
            Keep::Streams => {
                out[1] = 0x08 | if moves { 0x04 } else { 0 };
                let (base_e, base_p) = if delta { ([0.0; 3], [0.0; 3]) } else { (rest.euler, rest.position) };
                let mut streams: Vec<Vec<u8>> = Vec::new();
                for axis in 0..if moves { 6 } else { 3 } {
                    let quantized: Vec<i16> = frames
                        .iter()
                        .map(|&f| {
                            let (p, e) = values(f);
                            let v = if axis < 3 {
                                (e[axis] - base_e[axis]) / ROTATION_SCALE
                            } else {
                                (p.unwrap_or(base_p)[axis - 3] - base_p[axis - 3]) / POSITION_SCALE
                            };
                            v.round().clamp(-32767.0, 32767.0) as i16
                        })
                        .collect();
                    streams.push(if quantized.iter().all(|&v| v == 0) { Vec::new() } else { stream(&quantized) });
                }
                // a pointer of three offsets for each of rotation and position, the streams after
                let pointers = out.len();
                out.resize(pointers + if moves { 12 } else { 6 }, 0);
                for (axis, data) in streams.iter().enumerate() {
                    if data.is_empty() {
                        continue;
                    }
                    let pointer = pointers + if axis < 3 { 0 } else { 6 };
                    let offset = (out.len() - pointer) as i16;
                    let field = pointers + axis * 2;
                    out[field..field + 2].copy_from_slice(&offset.to_le_bytes());
                    out.extend(data);
                }
            }
            Keep::Raw48 | Keep::Raw64 => {
                let (p, e) = values(frames[0]);
                let p = p.unwrap_or(rest.position);
                let q = euler_quaternion(e);
                if animation.keep == Keep::Raw64 {
                    out[1] = 0x20 | 0x01;
                    let part = |v: f32| ((v as f64 * 1_048_576.5 + 1_048_576.0).round() as u64).min(0x1f_ffff);
                    let bits = part(q[0]) | part(q[1]) << 21 | part(q[2]) << 42 | u64::from(q[3] < 0.0) << 63;
                    out.extend(bits.to_le_bytes());
                } else {
                    out[1] = 0x02 | 0x01;
                    let wide = |v: f32| (v * 32768.0 + 32768.0).round().clamp(0.0, 65535.0) as u16;
                    let narrow = (q[2] * 16384.0 + 16384.0).round().clamp(0.0, 32767.0) as u16 | u16::from(q[3] < 0.0) << 15;
                    for word in [wide(q[0]), wide(q[1]), narrow] {
                        out.extend(word.to_le_bytes());
                    }
                }
                for v in p {
                    out.extend(to_half(v).to_le_bytes());
                }
            }
        }
        if delta {
            out[1] |= 0x10;
        }
        out
    }

    /// The records of every bone an animation moves, over some of its frames.
    fn records(&self, animation: &TestAnimation, frames: &[usize]) -> Vec<u8> {
        let mut bones: Vec<usize> = animation.frames.first().map(|f| f.iter().map(|(b, ..)| *b).collect()).unwrap_or_default();
        bones.sort_unstable();
        bones.dedup();
        let mut out = Vec::new();
        for (i, &bone) in bones.iter().enumerate() {
            let at = out.len();
            out.extend(self.record(animation, bone, frames));
            if i + 1 < bones.len() {
                let next = (out.len() - at) as i16;
                out[at + 2..at + 4].copy_from_slice(&next.to_le_bytes());
            }
        }
        if out.is_empty() {
            // an animation that moves nothing still has a record: the first bone's, unmoved
            out.extend([0, 0, 0, 0]);
        }
        out
    }

    /// The frames each section of an animation has: every `section_frames` frames and the one
    /// after (to go on to), and the last frame on its own.
    fn sections(animation: &TestAnimation) -> Vec<Vec<usize>> {
        let count = animation.frames.len();
        match animation.section_frames {
            0 => vec![(0..count).collect()],
            n => {
                let mut sections: Vec<Vec<usize>> =
                    (0..=count / n).map(|s| (s * n..=(s * n + n).min(count.saturating_sub(1))).collect()).collect();
                sections.push(vec![count.saturating_sub(1)]);
                sections
            }
        }
    }

    /// The `.mdl` file, and the `.ani` when an animation is kept in one of its blocks.
    pub fn files(&self) -> (Vec<u8>, Option<Vec<u8>>) {
        let mut b = Bytes::default();
        b.0.extend(b"IDST");
        b.i32(48).i32(self.checksum);
        b.zeros(64);
        let name: Vec<u8> = self.name.bytes().take(63).collect();
        b.0[12..12 + name.len()].copy_from_slice(&name);
        b.zeros(408 - b.at());
        let mut strings: Vec<(usize, usize, String)> = Vec::new();

        // bones, at rest where their parents put them
        let bone_at = b.at();
        let rest = self.rest_bone_to_model();
        for (i, bone) in self.bones.iter().enumerate() {
            let at = b.at();
            strings.push((at, at, bone.name.clone()));
            b.i32(0).i32(bone.parent).zeros(24);
            let q = euler_quaternion(bone.euler);
            b.f32s(&bone.position).f32s(&q).f32s(&bone.euler);
            b.f32s(&[POSITION_SCALE; 3]).f32s(&[ROTATION_SCALE; 3]);
            for row in invert(&rest[i]) {
                b.f32s(&row);
            }
            b.zeros(216 - (b.at() - at));
        }
        for (field, value) in [(156, self.bones.len() as i32), (160, bone_at as i32)] {
            b.set(field, value);
        }

        let attachment_at = b.at();
        for (name, bone, position) in &self.attachments {
            let at = b.at();
            strings.push((at, at, name.clone()));
            b.i32(0).i32(0).i32(*bone as i32);
            b.f32s(&[1.0, 0.0, 0.0, position[0], 0.0, 1.0, 0.0, position[1], 0.0, 0.0, 1.0, position[2]]);
            b.zeros(92 - (b.at() - at));
        }
        b.set(240, self.attachments.len() as i32);
        b.set(244, attachment_at as i32);

        let pose_at = b.at();
        for pose in &self.pose_parameters {
            let at = b.at();
            strings.push((at, at, pose.name.clone()));
            b.i32(0).i32(pose.flags).f32s(&[pose.start, pose.end, pose.wrap]);
        }
        b.set(300, self.pose_parameters.len() as i32);
        b.set(304, pose_at as i32);

        let include_at = b.at();
        for include in &self.include_models {
            let at = b.at();
            strings.push((at, at + 4, include.clone()));
            b.i32(0).i32(0);
        }
        b.set(336, self.include_models.len() as i32);
        b.set(340, include_at as i32);

        // the .ani: a header, then each block's records
        let blocks = self.animations.iter().map(|a| a.block).max().unwrap_or(0);
        let mut ani = Bytes::default();
        let mut block_ranges = vec![(0usize, 0usize); blocks + 1];
        let mut block_data: Vec<Vec<u8>> = vec![Vec::new(); blocks + 1];

        let anim_at = b.at();
        b.zeros(100 * self.animations.len());
        // for each animation, its sections: (block, offset within the block's data)
        let mut placed: Vec<Vec<(usize, usize)>> = Vec::new();
        for (i, animation) in self.animations.iter().enumerate() {
            let at = anim_at + i * 100;
            strings.push((at, at + 4, animation.name.clone()));
            b.set(at, -(at as i32));
            b.0[at + 8..at + 12].copy_from_slice(&animation.fps.to_le_bytes());
            b.set(at + 12, animation.flags);
            b.set(at + 16, animation.frames.len() as i32);
            if let Some(end) = animation.moves {
                b.set(at + 20, 1);
                b.point(at + 24, at);
                b.i32(animation.frames.len() as i32 - 1).i32(0).f32s(&[0.0, 0.0, 0.0]).f32s(&end).f32s(&end);
            }
            let mut sections = Vec::new();
            for frames in Self::sections(animation) {
                let records = self.records(animation, &frames);
                if animation.block == 0 {
                    sections.push((0, b.at()));
                    b.0.extend(records);
                } else {
                    sections.push((animation.block, block_data[animation.block].len()));
                    block_data[animation.block].extend(records);
                }
            }
            placed.push(sections);
        }
        // the section tables and the places of the records
        for (i, (animation, sections)) in self.animations.iter().zip(&placed).enumerate() {
            let at = anim_at + i * 100;
            let relative = |(block, offset): (usize, usize)| if block == 0 { offset as i32 - at as i32 } else { offset as i32 };
            b.set(at + 52, animation.block as i32);
            b.set(at + 56, relative(sections[0]));
            if animation.section_frames > 0 {
                b.set(at + 84, animation.section_frames as i32);
                b.point(at + 80, at);
                for &section in sections {
                    b.i32(section.0 as i32).i32(relative(section));
                }
            }
        }
        b.set(180, self.animations.len() as i32);
        b.set(184, anim_at as i32);

        let sequence_at = b.at();
        b.zeros(212 * self.sequences.len());
        for (i, sequence) in self.sequences.iter().enumerate() {
            let at = sequence_at + i * 212;
            strings.push((at, at + 4, sequence.label.clone()));
            strings.push((at, at + 8, sequence.activity.clone()));
            b.set(at, -(at as i32));
            b.set(at + 12, sequence.flags);
            b.set(at + 16, -1);
            b.set(at + 20, sequence.activity_weight);
            b.set(at + 56, sequence.animations.len() as i32);
            b.point(at + 60, at);
            for &animation in &sequence.animations {
                b.i16(animation as i16);
            }
            if b.at() % 4 != 0 {
                b.zeros(2);
            }
            b.set(at + 68, sequence.blend[0] as i32);
            b.set(at + 72, sequence.blend[1] as i32);
            b.set(at + 76, sequence.parameters[0]);
            b.set(at + 80, sequence.parameters[1]);
            for (k, v) in sequence.parameter_start.iter().chain(&sequence.parameter_end).enumerate() {
                b.0[at + 84 + k * 4..at + 88 + k * 4].copy_from_slice(&v.to_le_bytes());
            }
            for (field, v) in [(at + 104, 0.2f32), (at + 108, 0.2f32)] {
                b.0[field..field + 4].copy_from_slice(&v.to_le_bytes());
            }
            if !sequence.weights.is_empty() {
                b.point(at + 156, at);
                b.f32s(&sequence.weights);
            }
            b.set(at + 24, sequence.events.len() as i32);
            b.point(at + 28, at);
            for (cycle, event, options) in &sequence.events {
                let event_at = b.at();
                b.f32s(&[*cycle]).i32(*event).i32(0);
                b.zeros(64);
                b.0[event_at + 12..event_at + 12 + options.len().min(63)].copy_from_slice(&options.as_bytes()[..options.len().min(63)]);
                b.i32(0);
            }
            b.set(at + 148, sequence.layers.len() as i32);
            b.point(at + 152, at);
            for layer in &sequence.layers {
                b.i16(layer.sequence as i16).i16(layer.pose.map_or(-1, |p| p as i16));
                b.i32(layer.flags).f32s(&[layer.start, layer.peak, layer.tail, layer.end]);
            }
        }
        b.set(188, self.sequences.len() as i32);
        b.set(192, sequence_at as i32);

        if blocks > 0 {
            let name_at = b.string(&self.animation_file);
            b.set(348, name_at as i32);
            ani.0.extend(b"IDAG");
            ani.i32(48).i32(0).zeros(64);
            ani.i32(0);
            for (block, data) in block_data.iter().enumerate().skip(1) {
                let start = ani.at();
                ani.0.extend(data);
                block_ranges[block] = (start, ani.at());
            }
            let length = ani.at() as i32;
            ani.set(76, length);
            let table = b.at();
            for (start, end) in &block_ranges {
                b.i32(*start as i32).i32(*end as i32);
            }
            b.set(352, block_ranges.len() as i32);
            b.set(356, table as i32);
        }
        for (base, field, text) in strings {
            let at = b.string(&text);
            b.set(field, at as i32 - base as i32);
        }
        let length = b.at() as i32;
        b.set(76, length);
        b.set16(377, 0);
        (b.0, (blocks > 0).then_some(ani.0))
    }
}
