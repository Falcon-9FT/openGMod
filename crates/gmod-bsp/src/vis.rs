//! What is seen and heard from where: the visibility lump.
//!
//! The compiler (`vvis`) cuts the open space of a map into clusters of leaves and writes, for
//! each cluster, which clusters may be seen from it - the potentially visible set, PVS - and
//! which may be heard: those that any of the seen ones see, the PAS. The lump is the number of
//! clusters (`i32`), for each cluster the offsets of its two rows from the start of the lump
//! (`i32` PVS, `i32` PAS), and the rows: a bit for every cluster, the lowest bit of the first
//! byte for cluster 0, run-length encoded - a zero byte is followed by a byte of how many zero
//! bytes it stands for. A leaf names its cluster (`Leaf::cluster`), -1 when it is in none: in
//! the solid or outside the world.
//!
//! A map compiled without `vvis` has no lump: everything sees everything. So does a point in no
//! cluster - a player flying outside the world sees it all.

use crate::Bsp;
use crate::lumps::{Fields, index, lump_bytes};

/// The most clusters an entity's box may touch before it is taken as seen from everywhere.
pub const MAX_ENTITY_CLUSTERS: usize = 64;

/// A map's clusters, and what each sees and hears.
#[derive(Debug, Clone)]
pub struct Visibility {
    /// The world's tree: each node's plane (normal, distance) and children (`-1 - leaf` for a
    /// leaf).
    nodes: Vec<([f32; 3], f32, [i32; 2])>,
    head: i32,
    /// Each leaf's cluster, -1 for none.
    leaf_clusters: Vec<i16>,
    clusters: usize,
    /// Each cluster's rows, unpacked: a bit for every cluster.
    pvs: Vec<Vec<u8>>,
    pas: Vec<Vec<u8>>,
}

/// Unpacks a row at `at` of the lump into `bytes` bytes; `None` when it runs out of the lump.
fn unpack(lump: &[u8], at: usize, bytes: usize) -> Option<Vec<u8>> {
    let mut row = Vec::with_capacity(bytes);
    let mut i = at;
    while row.len() < bytes {
        let b = *lump.get(i)?;
        i += 1;
        if b == 0 {
            let run = *lump.get(i)? as usize;
            i += 1;
            row.extend(std::iter::repeat_n(0, run.min(bytes - row.len())));
        } else {
            row.push(b);
        }
    }
    Some(row)
}

/// Whether a row has a cluster's bit.
pub fn has(row: &[u8], cluster: usize) -> bool {
    row.get(cluster / 8).is_some_and(|b| b & (1 << (cluster % 8)) != 0)
}

impl Visibility {
    /// The visibility of a map; `None` when it has none (everything sees everything) or its lump
    /// does not add up - which costs the map its culling, not the map.
    pub fn new(bsp: &Bsp) -> Option<Visibility> {
        let lump = lump_bytes(bsp.raw(), &bsp.lumps[index::VISIBILITY], "visibility").ok()?;
        if lump.len() < 4 {
            return None;
        }
        let mut f = Fields::new(&lump);
        let clusters = usize::try_from(f.i32()).ok().filter(|&n| n > 0 && 4 + n * 8 <= lump.len())?;
        let bytes = clusters.div_ceil(8);
        let mut pvs = Vec::with_capacity(clusters);
        let mut pas = Vec::with_capacity(clusters);
        for _ in 0..clusters {
            let (seen, heard) = (usize::try_from(f.i32()).ok()?, usize::try_from(f.i32()).ok()?);
            pvs.push(unpack(&lump, seen, bytes)?);
            pas.push(unpack(&lump, heard, bytes)?);
        }
        let nodes = bsp
            .nodes
            .iter()
            .map(|n| {
                let plane = bsp.planes.get(usize::try_from(n.plane).ok()?)?;
                Some((plane.normal, plane.dist, n.children))
            })
            .collect::<Option<Vec<_>>>()?;
        let head = bsp.models.first()?.head_node;
        let leaf_clusters = bsp.leaves.iter().map(|l| if (l.cluster as isize) < clusters as isize { l.cluster } else { -1 }).collect();
        Some(Visibility { nodes, head, leaf_clusters, clusters, pvs, pas })
    }

    /// How many clusters the map has.
    pub fn cluster_count(&self) -> usize {
        self.clusters
    }

    /// The leaf a point is in.
    fn leaf_at(&self, point: [f32; 3]) -> Option<usize> {
        let mut node = self.head;
        let mut steps = 0;
        while let Ok(i) = usize::try_from(node) {
            let (normal, dist, children) = *self.nodes.get(i)?;
            let side = normal[0] * point[0] + normal[1] * point[1] + normal[2] * point[2] < dist;
            node = children[usize::from(side)];
            steps += 1;
            if steps > self.nodes.len() {
                return None;
            }
        }
        Some((-1 - node) as usize).filter(|&leaf| leaf < self.leaf_clusters.len())
    }

    /// The cluster a point is in; `None` in the solid or outside the world.
    pub fn cluster_at(&self, point: [f32; 3]) -> Option<usize> {
        let cluster = *self.leaf_clusters.get(self.leaf_at(point)?)?;
        usize::try_from(cluster).ok()
    }

    /// The clusters a box touches, each once, into `out`; `false` when there are more than
    /// [`MAX_ENTITY_CLUSTERS`] (`out` then has some of them).
    pub fn clusters_in_box(&self, mins: [f32; 3], maxs: [f32; 3], out: &mut Vec<usize>) -> bool {
        out.clear();
        let middle: [f32; 3] = std::array::from_fn(|i| (mins[i] + maxs[i]) * 0.5);
        let half: [f32; 3] = std::array::from_fn(|i| (maxs[i] - mins[i]).abs() * 0.5);
        let mut stack = vec![self.head];
        let mut visited = 0;
        while let Some(node) = stack.pop() {
            visited += 1;
            if visited > 4 * self.nodes.len() + 16 {
                return false;
            }
            match usize::try_from(node) {
                Ok(i) => {
                    let Some(&(normal, dist, children)) = self.nodes.get(i) else { continue };
                    let centre = normal[0] * middle[0] + normal[1] * middle[1] + normal[2] * middle[2] - dist;
                    let reach = normal[0].abs() * half[0] + normal[1].abs() * half[1] + normal[2].abs() * half[2];
                    if centre >= -reach {
                        stack.push(children[0]);
                    }
                    if centre < reach {
                        stack.push(children[1]);
                    }
                }
                Err(_) => {
                    let leaf = (-1 - node) as usize;
                    let Some(cluster) = self.leaf_clusters.get(leaf).and_then(|&c| usize::try_from(c).ok()) else { continue };
                    if !out.contains(&cluster) {
                        if out.len() == MAX_ENTITY_CLUSTERS {
                            return false;
                        }
                        out.push(cluster);
                    }
                }
            }
        }
        true
    }

    /// The clusters a cluster may see: a bit for each.
    pub fn pvs(&self, cluster: usize) -> &[u8] {
        self.pvs.get(cluster).map_or(&[], Vec::as_slice)
    }

    /// The clusters a cluster may hear: a bit for each.
    pub fn pas(&self, cluster: usize) -> &[u8] {
        self.pas.get(cluster).map_or(&[], Vec::as_slice)
    }
}
