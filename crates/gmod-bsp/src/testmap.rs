//! A small map written for the tests, lump by lump: a floor, a wall, a displacement, a sky face
//! and a nodraw face, with lightmaps of known colours, an LZMA-compressed entity lump, one
//! static prop and a pakfile.

use crate::lumps::{Overlay, index};

pub const RED: [u8; 4] = [255, 0, 0, 0];
pub const GREEN: [u8; 4] = [0, 255, 0, 0];
pub const BLUE: [u8; 4] = [0, 0, 255, 0];

fn f32s(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn i32s(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// A zip with stored entries.
fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in files {
        let crc = gmod_fs::crc32::crc32(data);
        let offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        directory.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        directory.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
        directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn face(plane: u16, first_edge: i32, texinfo: i16, dispinfo: i16, light_offset: i32, mins: [i32; 2], size: [i32; 2]) -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(&plane.to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&first_edge.to_le_bytes());
    f.extend_from_slice(&4i16.to_le_bytes());
    f.extend_from_slice(&texinfo.to_le_bytes());
    f.extend_from_slice(&dispinfo.to_le_bytes());
    f.extend_from_slice(&(-1i16).to_le_bytes());
    f.extend_from_slice(&[0, 255, 255, 255]);
    f.extend_from_slice(&light_offset.to_le_bytes());
    f.extend_from_slice(&f32s(&[128.0 * 128.0]));
    f.extend_from_slice(&i32s(&[mins[0], mins[1], size[0], size[1], -1]));
    f.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(f.len(), 56);
    f
}

fn texinfo(s: [f32; 4], t: [f32; 4], ls: [f32; 4], lt: [f32; 4], flags: i32, texdata: i32) -> Vec<u8> {
    let mut out = f32s(&[s, t, ls, lt].concat());
    out.extend_from_slice(&i32s(&[flags, texdata]));
    out
}

/// The map's bytes.
pub fn bytes() -> Vec<u8> {
    // clockwise seen from the front, as the game winds its faces
    let quads: [[[f32; 3]; 4]; 5] = [
        [[0.0, 0.0, 0.0], [0.0, 128.0, 0.0], [128.0, 128.0, 0.0], [128.0, 0.0, 0.0]],
        [[0.0, 0.0, 0.0], [128.0, 0.0, 0.0], [128.0, 0.0, 128.0], [0.0, 0.0, 128.0]],
        [[256.0, 0.0, 0.0], [256.0, 128.0, 0.0], [384.0, 128.0, 0.0], [384.0, 0.0, 0.0]],
        [[0.0, 0.0, 256.0], [0.0, 128.0, 256.0], [128.0, 128.0, 256.0], [128.0, 0.0, 256.0]],
        [[0.0, 128.0, 0.0], [128.0, 128.0, 0.0], [128.0, 128.0, 128.0], [0.0, 128.0, 128.0]],
    ];
    let vertices: Vec<f32> = quads.iter().flatten().flatten().copied().collect();
    // edge k joins vertex k to the next corner of its quad
    let mut edges = vec![0u8, 0, 0, 0];
    for q in 0..5u16 {
        for c in 0..4u16 {
            edges.extend_from_slice(&(q * 4 + c).to_le_bytes());
            edges.extend_from_slice(&(q * 4 + (c + 1) % 4).to_le_bytes());
        }
    }
    let surfedges: Vec<i32> = (1..=20).collect();
    let planes =
        f32s(&[0.0, 0.0, 1.0, 0.0]).into_iter().chain(i32s(&[2])).chain(f32s(&[0.0, 1.0, 0.0, 0.0])).chain(i32s(&[1])).collect::<Vec<u8>>();
    let sixteenth = 1.0 / 16.0;
    let texinfos = [
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [sixteenth, 0.0, 0.0, 0.0], [0.0, sixteenth, 0.0, 0.0], 0, 0),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [sixteenth, 0.0, 0.0, 0.0], [0.0, 0.0, sixteenth, 0.0], 0, 1),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [sixteenth, 0.0, 0.0, 0.0], [0.0, sixteenth, 0.0, 0.0], 0, 2),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4], [0.0; 4], crate::surf::SKY, 3),
        texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4], [0.0; 4], crate::surf::NODRAW, 4),
    ]
    .concat();
    let names = ["DEV/DEV_MEASUREGENERIC01", "BRICK/BRICKWALL001A", "NATURE/BLENDGROUNDTOGRASS", "TOOLS/TOOLSSKYBOX", "TOOLS/TOOLSNODRAW"];
    let mut string_data = Vec::new();
    let mut string_table = Vec::new();
    let mut texdata = Vec::new();
    for (i, name) in names.iter().enumerate() {
        string_table.extend_from_slice(&(string_data.len() as i32).to_le_bytes());
        string_data.extend_from_slice(name.as_bytes());
        string_data.push(0);
        texdata.extend_from_slice(&f32s(&[0.5, 0.5, 0.5]));
        texdata.extend_from_slice(&i32s(&[i as i32, 512, 256, 512, 256]));
    }
    let samples = 9 * 9;
    let faces = [
        face(0, 0, 0, -1, 0, [0, 0], [8, 8]),
        face(1, 4, 1, -1, samples * 4, [0, 0], [8, 8]),
        face(0, 8, 2, 0, samples * 8, [16, 0], [8, 8]),
        face(0, 12, 3, -1, -1, [0, 0], [0, 0]),
        face(1, 16, 4, -1, -1, [0, 0], [0, 0]),
    ]
    .concat();
    let lighting: Vec<u8> = [RED, GREEN, BLUE].iter().flat_map(|c| std::iter::repeat_n(*c, samples as usize).flatten()).collect();
    let models = [f32s(&[-1.0, -1.0, -1.0, 385.0, 129.0, 257.0, 0.0, 0.0, 0.0]), i32s(&[0, 0, 5])].concat();

    // a displacement of power 2 (5 x 5) on face 2, rising along its columns
    let mut dispinfo = f32s(&[256.0, 0.0, 0.0]);
    dispinfo.extend_from_slice(&i32s(&[0, 0, 2, 0]));
    dispinfo.extend_from_slice(&f32s(&[45.0]));
    dispinfo.extend_from_slice(&i32s(&[1]));
    dispinfo.extend_from_slice(&2u16.to_le_bytes());
    dispinfo.extend_from_slice(&[0, 0]);
    dispinfo.resize(176, 0);
    let mut disp_verts = Vec::new();
    for row in 0..5 {
        for column in 0..5 {
            disp_verts.extend_from_slice(&f32s(&[0.0, 0.0, 1.0, column as f32 * 4.0, row as f32 / 4.0 * 255.0]));
        }
    }

    // the entity lump, LZMA-compressed (made by Python's lzma module, raw LZMA1, dict 64 KiB)
    let entities = include_bytes!("../testdata/entities.txt");
    let stream = unhex(include_str!("../testdata/entities.lzma.hex"));
    let mut compressed = b"LZMA".to_vec();
    compressed.extend_from_slice(&(entities.len() as u32).to_le_bytes());
    compressed.extend_from_slice(&(stream.len() as u32).to_le_bytes());
    compressed.extend_from_slice(&[0x5D, 0x00, 0x00, 0x01, 0x00]);
    compressed.extend_from_slice(&stream);

    let pak = zip(&[("materials/maps/test/patched.vmt", b"\"patch\" { \"include\" \"materials/brick/brickwall001a.vmt\" }")]);
    let cubemaps = i32s(&[64, 64, 64, 0]);

    let lumps: Vec<(usize, Vec<u8>, u32)> = vec![
        (index::ENTITIES, compressed, entities.len() as u32),
        (index::PLANES, planes, 0),
        (index::TEXDATA, texdata, 0),
        (index::VERTEXES, f32s(&vertices), 0),
        (index::TEXINFO, texinfos, 0),
        (index::FACES, faces, 0),
        (index::LIGHTING, lighting, 0),
        (index::EDGES, edges, 0),
        (index::SURFEDGES, i32s(&surfedges), 0),
        (index::MODELS, models, 0),
        (index::DISPINFO, dispinfo, 0),
        (index::DISP_VERTS, disp_verts, 0),
        (index::PAKFILE, pak, 0),
        (index::CUBEMAPS, cubemaps, 0),
        (index::TEXDATA_STRING_DATA, string_data, 0),
        (index::TEXDATA_STRING_TABLE, string_table, 0),
    ];

    let mut out = vec![0u8; 8 + 64 * 16 + 4];
    out[0..4].copy_from_slice(b"VBSP");
    out[4..8].copy_from_slice(&20i32.to_le_bytes());
    let set_entry = |out: &mut Vec<u8>, index: usize, offset: usize, length: usize, fourcc: u32| {
        let at = 8 + index * 16;
        out[at..at + 4].copy_from_slice(&(offset as u32).to_le_bytes());
        out[at + 4..at + 8].copy_from_slice(&(length as u32).to_le_bytes());
        out[at + 12..at + 16].copy_from_slice(&fourcc.to_le_bytes());
    };
    for (index, data, fourcc) in lumps {
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let offset = out.len();
        out.extend_from_slice(&data);
        set_entry(&mut out, index, offset, data.len(), fourcc);
    }

    // the game lump: one sprp of version 6, then the closing empty entry
    let mut sprp = 1i32.to_le_bytes().to_vec();
    let mut name = b"models/props_c17/oildrum001.mdl".to_vec();
    name.resize(128, 0);
    sprp.extend_from_slice(&name);
    sprp.extend_from_slice(&i32s(&[1]));
    sprp.extend_from_slice(&7u16.to_le_bytes());
    sprp.extend_from_slice(&i32s(&[1]));
    sprp.extend_from_slice(&f32s(&[32.0, 64.0, 0.0, 0.0, 90.0, 0.0]));
    sprp.extend_from_slice(&[0, 0, 0, 0, 1, 0, 6, 0]);
    sprp.extend_from_slice(&i32s(&[1]));
    sprp.extend_from_slice(&f32s(&[100.0, 2000.0, 32.0, 64.0, 16.0, 1.0]));
    sprp.extend_from_slice(&[0, 0, 0, 0]);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
    let directory_at = out.len();
    let data_at = directory_at + 4 + 2 * 16;
    let mut game = 2i32.to_le_bytes().to_vec();
    game.extend_from_slice(&u32::from_be_bytes(*b"sprp").to_le_bytes());
    game.extend_from_slice(&0u16.to_le_bytes());
    game.extend_from_slice(&6u16.to_le_bytes());
    game.extend_from_slice(&i32s(&[data_at as i32, sprp.len() as i32]));
    game.extend_from_slice(&[0; 16]);
    game.extend_from_slice(&sprp);
    out.extend_from_slice(&game);
    set_entry(&mut out, index::GAME_LUMP, directory_at, game.len(), 0);
    out
}

/// A brush of a [`TestMap`]: a box, cut further by planes (the solid is behind each).
#[derive(Debug, Clone, PartialEq)]
pub struct TestBrush {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    /// Normals and distances.
    pub cuts: Vec<([f32; 3], f32)>,
    pub contents: u32,
    pub material: &'static str,
    /// `surf` flags of its sides.
    pub flags: i32,
}

impl TestBrush {
    /// A solid box.
    pub fn cuboid(mins: [f32; 3], maxs: [f32; 3]) -> TestBrush {
        TestBrush { mins, maxs, cuts: Vec::new(), contents: crate::contents::SOLID, material: "DEV/DEV_MEASUREGENERIC01", flags: 0 }
    }

    pub fn contents(mut self, contents: u32) -> TestBrush {
        self.contents = contents;
        self
    }

    pub fn material(mut self, material: &'static str, flags: i32) -> TestBrush {
        self.material = material;
        self.flags = flags;
        self
    }

    /// Cuts away what is in front of a plane.
    pub fn cut(mut self, normal: [f32; 3], dist: f32) -> TestBrush {
        let len = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        self.cuts.push((normal.map(|n| n / len), dist / len));
        self
    }
}

/// A displacement of a [`TestMap`]: a square grid flat at the height of `corner`, from there
/// along +x and +y, its points raised by `heights` (row by row; the rows go along +y, the
/// points of a row along +x).
#[derive(Debug, Clone, PartialEq)]
pub struct TestDisplacement {
    pub corner: [f32; 3],
    pub size: f32,
    pub power: i32,
    pub heights: Vec<f32>,
}

/// A light of a [`TestMap`] (`dworldlight_t`), falling off as one over the square of the
/// distance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestLight {
    /// `emittype_t`: 1 a point, 2 a spot, 3 the sun.
    pub kind: i32,
    pub origin: [f32; 3],
    pub intensity: [f32; 3],
    pub normal: [f32; 3],
}

/// A static prop of a [`TestMap`].
#[derive(Debug, Clone, PartialEq)]
pub struct TestStaticProp {
    pub model: String,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    /// How it collides ([`crate::props::solid`]).
    pub solid: u8,
}

/// A face of a [`TestMap`]'s world, without light map: its corners, clockwise seen from the
/// front as the game winds them.
#[derive(Debug, Clone, PartialEq)]
pub struct TestFace {
    pub corners: Vec<[f32; 3]>,
    pub material: &'static str,
    /// `surf` flags.
    pub flags: i32,
}

/// An overlay of a [`TestMap`], its material by name.
#[derive(Debug, Clone, PartialEq)]
pub struct TestOverlay {
    pub material: &'static str,
    /// The record as written, but for its texinfo: the material's.
    pub overlay: Overlay,
}

impl TestOverlay {
    /// A square `size` units a side around `origin`, facing `normal` with its U axis `u`, the
    /// whole texture once, on the map's faces `faces` (the displacements' first, then
    /// [`TestMap::faces`]).
    pub fn square(material: &'static str, faces: Vec<i32>, origin: [f32; 3], normal: [f32; 3], u: [f32; 3], size: f32) -> TestOverlay {
        let h = size / 2.0;
        let points = [[-h, -h, u[0]], [-h, h, u[1]], [h, h, u[2]], [h, -h, 0.0]];
        let overlay = Overlay { id: 0, texinfo: 0, render_order: 0, faces, u: [0.0, 1.0], v: [0.0, 1.0], points, origin, normal };
        TestOverlay { material, overlay }
    }
}

/// A map of brushes for tests, lump by lump: the world and brush entities, each with a BSP
/// tree split along its brushes' bounds, displacements and faces in the world, static props,
/// and entities.
#[derive(Debug, Clone, Default)]
pub struct TestMap {
    /// The brushes of each brush model; the first are the world's.
    pub models: Vec<Vec<TestBrush>>,
    pub displacements: Vec<TestDisplacement>,
    pub faces: Vec<TestFace>,
    /// Faces of the brush models after the first, in the model's own space: (model, face).
    pub model_faces: Vec<(usize, TestFace)>,
    /// Areas of the world's leaves: a leaf whose middle is inside a box (mins, maxs) is in its
    /// area; the others in none (0).
    pub areas: Vec<([f32; 3], [f32; 3], u16)>,
    pub static_props: Vec<TestStaticProp>,
    /// The entities after `worldspawn`, as the entity lump writes them.
    pub entities: String,
    /// The leaves as maps of version 19 write them, with an ambient light cube each.
    pub old_leaves: bool,
    /// One ambient light sample in the middle of every leaf, of this cube (`ColorRGBExp32`
    /// for +x, -x, +y, -y, +z, -z), in the lumps of version 20.
    pub ambient: Option<[[u8; 4]; 6]>,
    /// The same in the HDR lumps.
    pub ambient_hdr: Option<[[u8; 4]; 6]>,
    pub lights: Vec<TestLight>,
    pub overlays: Vec<TestOverlay>,
}

/// A brush as the tree builder sees it: its index and bounds.
type Boxed = (usize, [f32; 3], [f32; 3]);

#[derive(Default)]
struct Tree {
    planes: Vec<u8>,
    plane_count: usize,
    nodes: Vec<u8>,
    node_count: usize,
    leaves: Vec<u8>,
    leaf_count: usize,
    leaf_brushes: Vec<u8>,
    leaf_brush_count: usize,
    old_leaves: bool,
    areas: Vec<([f32; 3], [f32; 3], u16)>,
}

impl Tree {
    fn plane(&mut self, normal: [f32; 3], dist: f32) -> usize {
        let axial = normal.iter().filter(|n| **n == 0.0).count() == 2;
        let dominant = (0..3).max_by(|&a, &b| normal[a].abs().total_cmp(&normal[b].abs())).unwrap_or(0) as i32;
        self.planes.extend(f32s(&[normal[0], normal[1], normal[2], dist]));
        self.planes.extend(i32s(&[if axial { dominant } else { 3 + dominant }]));
        self.plane_count += 1;
        self.plane_count - 1
    }

    fn leaf(&mut self, brushes: &[Boxed], contents: &[u32], region: ([f32; 3], [f32; 3])) -> i32 {
        let mut leaf = brushes.iter().fold(0u32, |c, b| c | contents[b.0]).to_le_bytes().to_vec();
        leaf.extend(0i16.to_le_bytes()); // cluster
        let middle: [f32; 3] = std::array::from_fn(|i| (region.0[i] + region.1[i]) / 2.0);
        let inside = |(lo, hi, _): &&([f32; 3], [f32; 3], u16)| (0..3).all(|i| lo[i] <= middle[i] && middle[i] <= hi[i]);
        // leaf 0 is outside everything
        let area = self.areas.iter().find(inside).filter(|_| self.leaf_count > 0).map_or(0, |a| a.2);
        leaf.extend(area.to_le_bytes()); // area, and no flags
        for v in region.0.iter().chain(&region.1) {
            leaf.extend((v.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
        leaf.extend([0, 0, 0, 0]); // leaf faces
        leaf.extend((self.leaf_brush_count as u16).to_le_bytes());
        leaf.extend((brushes.len() as u16).to_le_bytes());
        leaf.extend((-1i16).to_le_bytes()); // water data
        leaf.resize(if self.old_leaves { 56 } else { 32 }, 0);
        self.leaves.extend(leaf);
        for b in brushes {
            self.leaf_brushes.extend((b.0 as u16).to_le_bytes());
        }
        self.leaf_brush_count += brushes.len();
        self.leaf_count += 1;
        -(self.leaf_count as i32)
    }

    /// The node over `brushes` in `region`, its subtree written after it; a leaf when there is
    /// nothing left to split, unless a node is needed (a model's head).
    fn build(&mut self, brushes: &[Boxed], contents: &[u32], region: ([f32; 3], [f32; 3]), depth: usize, node_needed: bool) -> i32 {
        // the split that best halves the brushes, at a brush's side inside the region
        let mut best: Option<(usize, f32, usize)> = None;
        if brushes.len() > 1 && depth < 24 {
            for axis in 0..3 {
                for b in brushes {
                    for value in [b.1[axis], b.2[axis]] {
                        if value <= region.0[axis] || value >= region.1[axis] {
                            continue;
                        }
                        let front = brushes.iter().filter(|o| o.2[axis] >= value).count();
                        let back = brushes.iter().filter(|o| o.1[axis] <= value).count();
                        let worst = front.max(back);
                        if worst < brushes.len() && best.is_none_or(|(_, _, w)| worst < w) {
                            best = Some((axis, value, worst));
                        }
                    }
                }
            }
        }
        // the areas split the tree too, so that no leaf is partly in one
        let overlaps = |(lo, hi, _): &&([f32; 3], [f32; 3], u16)| (0..3).all(|i| lo[i] < region.1[i] && hi[i] > region.0[i]);
        let area_side = self
            .areas
            .iter()
            .filter(overlaps)
            .flat_map(|(lo, hi, _)| (0..3).flat_map(move |axis| [(axis, lo[axis]), (axis, hi[axis])]))
            .find(|&(axis, value)| region.0[axis] < value && value < region.1[axis]);
        let (axis, value) = match area_side.or(best.map(|(axis, value, _)| (axis, value))) {
            Some((axis, value)) => (axis, value),
            None if node_needed => (0, (region.0[0] + region.1[0]) / 2.0),
            None => return self.leaf(brushes, contents, region),
        };
        let index = self.node_count;
        self.node_count += 1;
        let at = self.nodes.len();
        self.nodes.resize(at + 32, 0);
        let mut normal = [0.0; 3];
        normal[axis] = 1.0;
        let plane = self.plane(normal, value);
        let in_front: Vec<Boxed> = brushes.iter().copied().filter(|b| b.2[axis] >= value).collect();
        let behind: Vec<Boxed> = brushes.iter().copied().filter(|b| b.1[axis] <= value).collect();
        let (mut front_region, mut back_region) = (region, region);
        front_region.0[axis] = value;
        back_region.1[axis] = value;
        let front = self.build(&in_front, contents, front_region, depth + 1, false);
        let back = self.build(&behind, contents, back_region, depth + 1, false);
        let mut node = i32s(&[plane as i32, front, back]);
        for v in region.0.iter().chain(&region.1) {
            node.extend((v.clamp(-32768.0, 32767.0) as i16).to_le_bytes());
        }
        node.resize(32, 0);
        self.nodes[at..at + 32].copy_from_slice(&node);
        index as i32
    }
}

impl TestMap {
    /// The map's bytes.
    pub fn bytes(&self) -> Vec<u8> {
        let mut tree = Tree { old_leaves: self.old_leaves, areas: self.areas.clone(), ..Tree::default() };
        // leaf 0, as compilers write it: outside everything
        tree.leaf(&[], &[], ([0.0; 3], [0.0; 3]));

        let mut names: Vec<&str> = Vec::new();
        let mut texinfos: Vec<(i32, usize)> = Vec::new();
        let mut surface = |material: &'static str, flags: i32| -> i16 {
            let texdata = names.iter().position(|n| *n == material).unwrap_or_else(|| {
                names.push(material);
                names.len() - 1
            });
            let index = texinfos.iter().position(|t| *t == (flags, texdata)).unwrap_or_else(|| {
                texinfos.push((flags, texdata));
                texinfos.len() - 1
            });
            index as i16
        };

        let (mut brushes, mut sides) = (Vec::new(), Vec::new());
        let (mut brush_count, mut side_count) = (0usize, 0usize);
        let mut contents = Vec::new();
        let mut models = Vec::new();
        let models_in = if self.models.is_empty() { vec![Vec::new()] } else { self.models.clone() };
        for (m, model) in models_in.iter().enumerate() {
            let mut boxed = Vec::new();
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for brush in model {
                let side_surface = surface(brush.material, brush.flags);
                let mut planes = Vec::new();
                for axis in 0..3 {
                    let mut normal = [0.0; 3];
                    normal[axis] = 1.0;
                    planes.push((normal, brush.maxs[axis]));
                    normal[axis] = -1.0;
                    planes.push((normal, -brush.mins[axis]));
                }
                planes.extend(brush.cuts.iter().copied());
                brushes.extend(i32s(&[side_count as i32, planes.len() as i32, brush.contents as i32]));
                for (normal, dist) in planes {
                    let plane = tree.plane(normal, dist);
                    sides.extend((plane as u16).to_le_bytes());
                    sides.extend(side_surface.to_le_bytes());
                    sides.extend([0, 0, 0, 0]); // no displacement, no bevel
                    side_count += 1;
                }
                boxed.push((brush_count, brush.mins, brush.maxs));
                contents.push(brush.contents);
                brush_count += 1;
                lo = std::array::from_fn(|i| lo[i].min(brush.mins[i]));
                hi = std::array::from_fn(|i| hi[i].max(brush.maxs[i]));
            }
            if model.is_empty() {
                (lo, hi) = ([-1.0; 3], [1.0; 3]);
            }
            let region = (lo.map(|v| v - 1.0), hi.map(|v| v + 1.0));
            let head = tree.build(&boxed, &contents, region, 0, true);
            // areas are the world's
            tree.areas.clear();
            // the world's faces first, then each model's in turn
            let world_faces = self.displacements.len() + self.faces.len();
            let (first, faces) = match m {
                0 => (0, world_faces),
                _ => {
                    let before = self.model_faces.iter().filter(|(model, _)| (1..m).contains(model)).count();
                    (world_faces + before, self.model_faces.iter().filter(|(model, _)| *model == m).count())
                }
            };
            models.extend(f32s(&[lo, hi, [0.0; 3]].concat()));
            models.extend(i32s(&[head, first as i32, faces as i32]));
        }

        // the displacements: a four-sided face each, and the grid over it
        let (mut vertices, mut edges, mut surfedges, mut faces) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut dispinfo, mut disp_verts) = (Vec::new(), Vec::new());
        let mut vert_count = 0;
        for (k, disp) in self.displacements.iter().enumerate() {
            let [x, y, z] = disp.corner;
            let s = disp.size;
            let first_vertex = vertices.len() / 12;
            // clockwise seen from above
            vertices.extend(f32s(&[x, y, z, x, y + s, z, x + s, y + s, z, x + s, y, z]));
            let first_edge = surfedges.len() / 4;
            for c in 0..4 {
                let edge = edges.len() / 4;
                edges.extend(((first_vertex + c) as u16).to_le_bytes());
                edges.extend(((first_vertex + (c + 1) % 4) as u16).to_le_bytes());
                surfedges.extend((edge as i32).to_le_bytes());
            }
            let plane = tree.plane([0.0, 0.0, 1.0], z);
            faces.extend(face(plane as u16, first_edge as i32, surface("NATURE/BLENDGROUNDTOGRASS", 0), k as i16, -1, [0, 0], [0, 0]));
            let side = (1usize << disp.power) + 1;
            assert_eq!(disp.heights.len(), side * side, "heights of a displacement of power {}", disp.power);
            let mut info = f32s(&[x, y, z]);
            info.extend(i32s(&[vert_count as i32, 0, disp.power, 0]));
            info.extend(f32s(&[45.0]));
            info.extend(i32s(&[crate::contents::SOLID as i32]));
            info.extend((k as u16).to_le_bytes());
            info.resize(176, 0);
            dispinfo.extend(info);
            for h in &disp.heights {
                disp_verts.extend(f32s(&[0.0, 0.0, 1.0, *h, 0.0]));
            }
            vert_count += side * side;
        }
        let mut model_faces: Vec<&(usize, TestFace)> = self.model_faces.iter().filter(|(m, _)| *m > 0 && *m < self.models.len()).collect();
        model_faces.sort_by_key(|(m, _)| *m);
        for test_face in self.faces.iter().chain(model_faces.into_iter().map(|(_, face)| face)) {
            let first_vertex = vertices.len() / 12;
            let first_edge = surfedges.len() / 4;
            let n = test_face.corners.len();
            for (c, corner) in test_face.corners.iter().enumerate() {
                vertices.extend(f32s(corner));
                let edge = edges.len() / 4;
                edges.extend(((first_vertex + c) as u16).to_le_bytes());
                edges.extend(((first_vertex + (c + 1) % n) as u16).to_le_bytes());
                surfedges.extend((edge as i32).to_le_bytes());
            }
            // clockwise from the front: the normal is the second edge across the first
            let [a, b, c] = [0, 1, 2].map(|i| test_face.corners[i]);
            let (ab, ac) = (crate::math::sub(b, a), crate::math::sub(c, a));
            let normal = crate::math::normalize(crate::math::cross(ac, ab));
            let plane = tree.plane(normal, crate::math::dot(normal, a));
            let texinfo = surface(test_face.material, test_face.flags);
            let mut bytes = face(plane as u16, first_edge as i32, texinfo, -1, -1, [0, 0], [0, 0]);
            bytes[8..10].copy_from_slice(&(n as i16).to_le_bytes()); // the number of edges
            faces.extend(bytes);
        }

        let overlays: Vec<u8> =
            self.overlays.iter().flat_map(|o| Overlay { texinfo: surface(o.material, 0), ..o.overlay.clone() }.bytes()).collect();

        let mut string_data = Vec::new();
        let mut string_table = Vec::new();
        let mut texdata = Vec::new();
        for (i, name) in names.iter().enumerate() {
            string_table.extend((string_data.len() as i32).to_le_bytes());
            string_data.extend(name.as_bytes());
            string_data.push(0);
            texdata.extend(f32s(&[0.5, 0.5, 0.5]));
            texdata.extend(i32s(&[i as i32, 64, 64, 64, 64]));
        }
        let texinfo_bytes: Vec<u8> = texinfos
            .iter()
            .flat_map(|&(flags, texdata)| texinfo([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4], [0.0; 4], flags, texdata as i32))
            .collect();
        let entities = format!("{{\n\"classname\" \"worldspawn\"\n}}\n{}", self.entities).into_bytes();

        let ambient_lumps = |cube: Option<[[u8; 4]; 6]>| {
            let (mut index, mut samples) = (Vec::new(), Vec::new());
            if let Some(cube) = cube {
                for leaf in 0..tree.leaf_count {
                    index.extend(1u16.to_le_bytes());
                    index.extend((leaf as u16).to_le_bytes());
                    samples.extend(cube.iter().flatten());
                    samples.extend([128, 128, 128, 0]);
                }
            }
            (index, samples)
        };
        let (ambient_index, ambient_samples) = ambient_lumps(self.ambient);
        let (ambient_index_hdr, ambient_samples_hdr) = ambient_lumps(self.ambient_hdr);
        let mut world_lights = Vec::new();
        for light in &self.lights {
            world_lights.extend(f32s(&[light.origin, light.intensity, light.normal].concat()));
            // cluster, type, style
            world_lights.extend(i32s(&[0, light.kind, 0]));
            // stopdot, stopdot2, exponent, radius, constant, linear, quadratic
            world_lights.extend(f32s(&[0.9, 0.8, 1.0, 0.0, 0.0, 0.0, 1.0]));
            // flags, texinfo, owner
            world_lights.extend(i32s(&[0, -1, 0]));
        }

        let lumps: Vec<(usize, Vec<u8>, u32)> = vec![
            (15, world_lights, 0),
            (51, ambient_index_hdr, 0),
            (52, ambient_index, 0),
            (55, ambient_samples_hdr, 0),
            (56, ambient_samples, 0),
            (index::ENTITIES, entities, 0),
            (index::PLANES, tree.planes, 0),
            (index::TEXDATA, texdata, 0),
            (index::VERTEXES, vertices, 0),
            (index::NODES, tree.nodes, 0),
            (index::TEXINFO, texinfo_bytes, 0),
            (index::FACES, faces, 0),
            (index::LEAFS, tree.leaves, if self.old_leaves { 0 } else { 1 }),
            (index::EDGES, edges, 0),
            (index::SURFEDGES, surfedges, 0),
            (index::MODELS, models, 0),
            (index::LEAF_BRUSHES, tree.leaf_brushes, 0),
            (index::BRUSHES, brushes, 0),
            (index::BRUSH_SIDES, sides, 0),
            (index::DISPINFO, dispinfo, 0),
            (index::DISP_VERTS, disp_verts, 0),
            (index::TEXDATA_STRING_DATA, string_data, 0),
            (index::TEXDATA_STRING_TABLE, string_table, 0),
            (index::OVERLAYS, overlays, 0),
        ];
        let mut out = vec![0u8; 8 + 64 * 16 + 4];
        out[0..4].copy_from_slice(b"VBSP");
        out[4..8].copy_from_slice(&20i32.to_le_bytes());
        let set_entry = |out: &mut Vec<u8>, index: usize, offset: usize, length: usize, version: u32| {
            let at = 8 + index * 16;
            out[at..at + 16].copy_from_slice(&i32s(&[offset as i32, length as i32, version as i32, 0]));
        };
        for (index, data, version) in lumps {
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            let at = out.len();
            set_entry(&mut out, index, at, data.len(), version);
            out.extend(data);
        }

        // the static props: a game lump of one sprp of version 6
        if !self.static_props.is_empty() {
            let mut models: Vec<&str> = Vec::new();
            for prop in &self.static_props {
                if !models.contains(&prop.model.as_str()) {
                    models.push(&prop.model);
                }
            }
            let mut sprp = i32s(&[models.len() as i32]);
            for model in &models {
                let mut name = model.as_bytes().to_vec();
                name.resize(128, 0);
                sprp.extend(name);
            }
            sprp.extend(i32s(&[0, self.static_props.len() as i32]));
            for prop in &self.static_props {
                sprp.extend(f32s(&[prop.origin, prop.angles].concat()));
                let model = models.iter().position(|m| *m == prop.model).unwrap_or(0) as u16;
                sprp.extend(model.to_le_bytes());
                sprp.extend([0, 0, 0, 0, prop.solid, 0]);
                sprp.extend(i32s(&[0]));
                sprp.extend(f32s(&[0.0, 0.0, 0.0, 0.0, 0.0, 1.0]));
                sprp.extend([0; 4]);
            }
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            let directory_at = out.len();
            let data_at = directory_at + 4 + 16;
            let mut game = i32s(&[1]);
            game.extend(u32::from_be_bytes(*b"sprp").to_le_bytes());
            game.extend(0u16.to_le_bytes());
            game.extend(6u16.to_le_bytes());
            game.extend(i32s(&[data_at as i32, sprp.len() as i32]));
            game.extend(sprp);
            set_entry(&mut out, index::GAME_LUMP, directory_at, game.len(), 0);
            out.extend(game);
        }
        out
    }
}
