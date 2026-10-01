//! Small model files written for tests, also those of crates that read models (feature
//! `testmodel`): a model header, and physics models of convex pieces.

use crate::phy::to_ivp;

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
