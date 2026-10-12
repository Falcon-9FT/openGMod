//! The node graph NPCs find their way by. A map places nodes - `info_node` on the ground,
//! `info_node_hint` a ground node with a hint for the AI, `info_node_air` and
//! `info_node_air_hint` in the air - and, as in HL2, they are no entities: the engine takes where
//! they are as the map loads (so `ents.FindByClass("info_node")` finds none), and a ground node
//! is put on the floor below it.
//!
//! When an NPC first needs a way, the nodes are linked for its hull: two ground nodes near each
//! other where the hull walks from one to the other - up a step, down a step, not off a ledge
//! nor up a slope too steep - and two air nodes where it flies straight. HL2 keeps the graph it
//! built in the map's `.ain` file; this one is built anew, from the world alone: doors and what
//! moves are met on the way.
//!
//! A way from one point to another ([`route`]) is a straight walk when there is one; else
//! from a node near the start the NPC walks to, along the links (A*, by distance), to a node
//! near the goal that walks to it.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use std::rc::Rc;

use mlua::Lua;

use crate::entity::EntityHandle;
use crate::host;
use crate::trace::{self, Filter, Hit, Sweep};
use crate::vector::Vector;

/// How high a step an NPC walks up (and down) without jumping, as a player's.
pub(crate) const STEP: f32 = 18.0;
/// How far apart two nodes may be to be linked.
const MAX_LINK: f32 = 720.0;
/// How many of the nearest nodes each node is tried with.
const NEIGHBOURS: usize = 24;
/// How long a stride of a walk that is looked at is.
const STRIDE: f32 = 16.0;
/// How steep a floor is walked on: the least upward part of its normal.
const FLOOR: f32 = 0.7;
/// How many nodes near the start and near the goal a way may begin and end at.
const ENDS: usize = 6;
/// How far below a node or a goal the floor is looked for.
const DROP: f32 = 384.0;
/// How far above where it stands a walk is looked at from, clear of the floor it touches.
const LIFT: f32 = 0.0625;

/// Where a node of the map is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Node {
    pub pos: Vector,
    pub air: bool,
}

/// The box an NPC moves as, and whether it flies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hull {
    pub mins: Vector,
    pub maxs: Vector,
    pub air: bool,
}

impl Hull {
    fn key(&self) -> [i32; 7] {
        let r = |v: f32| v.round() as i32;
        [r(self.mins.x), r(self.mins.y), r(self.mins.z), r(self.maxs.x), r(self.maxs.y), r(self.maxs.z), self.air as i32]
    }
}

/// What a walk or a flight runs into: only the world, or every entity but these.
#[derive(Debug, Clone)]
pub(crate) enum Solid {
    World,
    AllBut(Vec<EntityHandle>),
}

/// Each node's links, to a node and how far it is.
type Links = Vec<Vec<(usize, f32)>>;

/// The nodes of the map, and their links for each hull asked for.
#[derive(Default)]
pub(crate) struct Graph {
    nodes: RefCell<Vec<Node>>,
    links: RefCell<HashMap<[i32; 7], Rc<Links>>>,
}

/// Whether an entity class of a map is a node, and of the air.
pub(crate) fn node_class(class: &str) -> Option<bool> {
    match class.to_ascii_lowercase().as_str() {
        "info_node" | "info_node_hint" => Some(false),
        "info_node_air" | "info_node_air_hint" => Some(true),
        _ => None,
    }
}

/// The map's nodes, as it loads ([`crate::ents::spawn_map`]): each ground node on the floor, and
/// the links for people.
pub(crate) fn load(lua: &Lua, nodes: Vec<Node>) {
    let nodes: Vec<Node> = nodes
        .into_iter()
        .map(|node| if node.air { node } else { Node { pos: floor_below(lua, node.pos).unwrap_or(node.pos), ..node } })
        .collect();
    let graph = &host(lua).nav;
    let ground = nodes.iter().any(|n| !n.air);
    *graph.nodes.borrow_mut() = nodes;
    graph.links.borrow_mut().clear();
    // the links of people, whom most NPCs are, now rather than in the middle of the game
    if ground {
        links(lua, &HUMAN);
    }
}

/// The hull of people (`HULL_HUMAN`).
const HUMAN: Hull = Hull { mins: Vector::new(-13.0, -13.0, 0.0), maxs: Vector::new(13.0, 13.0, 72.0), air: false };

/// The nodes of the map.
pub(crate) fn nodes(lua: &Lua) -> Vec<Node> {
    host(lua).nav.nodes.borrow().clone()
}

fn up(z: f32) -> Vector {
    Vector::new(0.0, 0.0, z)
}

fn flat(v: Vector) -> Vector {
    Vector::new(v.x, v.y, 0.0)
}

/// A trace of a hull (a line for none) that hits what `solid` says, as an NPC moves.
pub(crate) fn sweep(lua: &Lua, from: Vector, to: Vector, hull: Option<&Hull>, solid: &Solid) -> Hit {
    let (filter, whitelist) = match solid {
        Solid::World => (Filter::Entities(Vec::new()), true),
        Solid::AllBut(list) => (Filter::Entities(list.clone()), false),
    };
    let (mins, maxs) = hull.map_or_else(Default::default, |h| (h.mins, h.maxs));
    let sweep = Sweep { mins, maxs, mask: gmod_bsp::mask::NPCSOLID, whitelist, group: trace::group::NPC, ..Sweep::line(from, to, filter) };
    trace::trace(lua, &sweep)
}

/// The floor a point is above, within [`DROP`]: where a line down meets it.
pub(crate) fn floor_below(lua: &Lua, point: Vector) -> Option<Vector> {
    let hit = sweep(lua, point + up(1.0), point - up(DROP), None, &Solid::World);
    (hit.fraction < 1.0 && !hit.start_solid).then_some(hit.end)
}

/// Why a stride did not go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Stop {
    /// Something stands in the way, facing so.
    Blocked(Vector),
    /// There is no floor to step onto, or it is too steep.
    Ledge,
}

/// One stride on the ground from where a hull stands across `delta` (flat): up a step, across,
/// and down onto the floor there, at most a step below. Where it stands after.
pub(crate) fn stride(lua: &Lua, from: Vector, delta: Vector, hull: &Hull, solid: &Solid) -> Result<Vector, Stop> {
    // a box standing right on the floor touches it: from a hair above
    let from = from + up(LIFT);
    let raised = sweep(lua, from, from + up(STEP), Some(hull), solid);
    let top = if raised.start_solid { from } else { raised.end };
    let across = sweep(lua, top, top + flat(delta), Some(hull), solid);
    if across.start_solid || across.fraction < 1.0 {
        return Err(Stop::Blocked(across.normal));
    }
    let down = sweep(lua, across.end, across.end - up(top.z - from.z + STEP), Some(hull), solid);
    if down.start_solid || down.fraction >= 1.0 || down.normal.z < FLOOR {
        return Err(Stop::Ledge);
    }
    Ok(down.end)
}

/// Whether a hull walks from one point on the ground to another, by strides.
fn walks(lua: &Lua, from: Vector, to: Vector, hull: &Hull, solid: &Solid) -> bool {
    if quick_walk(lua, from, to, hull, solid) {
        return true;
    }
    let mut at = from;
    let strides = ((flat(to - from).length() / STRIDE).ceil() as usize).max(1) + 2;
    for _ in 0..strides {
        let left = flat(to - at);
        let length = left.length();
        if length < 0.5 {
            break;
        }
        match stride(lua, at, left * (STRIDE.min(length) / length), hull, solid) {
            Ok(next) => at = next,
            Err(_) => return false,
        }
    }
    flat(to - at).length() < 1.0 && (at.z - to.z).abs() <= STEP
}

/// A walk looked at all at once: the hull goes straight a step up from one end to the other,
/// and under the way there is floor all along, no step higher than a step. What this does not
/// pass (stairs that rise above the line) is looked at stride by stride.
fn quick_walk(lua: &Lua, from: Vector, to: Vector, hull: &Hull, solid: &Solid) -> bool {
    let line = sweep(lua, from + up(STEP + LIFT), to + up(STEP + LIFT), Some(hull), solid);
    if line.start_solid || line.fraction < 1.0 {
        return false;
    }
    let samples = ((flat(to - from).length() / STRIDE).ceil() as usize).max(1);
    let mut last = from.z;
    for i in 1..=samples {
        let point = from + (to - from) * (i as f32 / samples as f32);
        let down = sweep(lua, point + up(STEP), point - up(STEP * 2.0), None, solid);
        if down.start_solid || down.fraction >= 1.0 || down.normal.z < FLOOR || (down.end.z - last).abs() > STEP {
            return false;
        }
        last = down.end.z;
    }
    (last - to.z).abs() <= STEP
}

/// Whether a hull flies straight from one point to another.
fn flies(lua: &Lua, from: Vector, to: Vector, hull: &Hull, solid: &Solid) -> bool {
    let hit = sweep(lua, from, to, Some(hull), solid);
    !hit.start_solid && hit.fraction >= 1.0
}

/// Whether a hull gets straight from one point to another: walking, or flying for one that flies.
pub(crate) fn reaches(lua: &Lua, from: Vector, to: Vector, hull: &Hull, solid: &Solid) -> bool {
    if hull.air { flies(lua, from, to, hull, solid) } else { walks(lua, from, to, hull, solid) }
}

/// Whether a hull fits where a node is.
fn fits(lua: &Lua, at: Vector, hull: &Hull) -> bool {
    let lifted = at + up(LIFT);
    !sweep(lua, lifted, lifted, Some(hull), &Solid::World).start_solid
}

/// The links of the nodes for a hull, built the first time it is asked for.
fn links(lua: &Lua, hull: &Hull) -> Rc<Links> {
    if let Some(links) = host(lua).nav.links.borrow().get(&hull.key()) {
        return links.clone();
    }
    let nodes = nodes(lua);
    let usable: Vec<bool> = nodes.iter().map(|n| n.air == hull.air && fits(lua, n.pos, hull)).collect();
    // each pair once, of the nearest nodes of each
    let mut pairs = BTreeSet::new();
    for (i, node) in nodes.iter().enumerate().filter(|(i, _)| usable[*i]) {
        let mut near: Vec<(usize, f32)> = nodes
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i && usable[*j])
            .map(|(j, other)| (j, (other.pos - node.pos).length()))
            .filter(|(_, d)| *d <= MAX_LINK)
            .collect();
        near.sort_by(|a, b| a.1.total_cmp(&b.1));
        pairs.extend(near.into_iter().take(NEIGHBOURS).map(|(j, _)| (i.min(j), i.max(j))));
    }
    let mut links: Links = vec![Vec::new(); nodes.len()];
    for (i, j) in pairs {
        let (a, b) = (nodes[i].pos, nodes[j].pos);
        let d = (b - a).length();
        if reaches(lua, a, b, hull, &Solid::World) && reaches(lua, b, a, hull, &Solid::World) {
            links[i].push((j, d));
            links[j].push((i, d));
        }
    }
    let links = Rc::new(links);
    host(lua).nav.links.borrow_mut().insert(hull.key(), links.clone());
    links
}

/// The nodes a hull may use nearest a point, nearest first, with how far they are.
fn nearest(nodes: &[Node], links: &Links, point: Vector, hull: &Hull) -> Vec<(usize, f32)> {
    let mut near: Vec<(usize, f32)> = nodes
        .iter()
        .enumerate()
        .filter(|(i, n)| n.air == hull.air && !links[*i].is_empty())
        .map(|(i, n)| (i, (n.pos - point).length()))
        .filter(|(_, d)| *d <= MAX_LINK)
        .collect();
    near.sort_by(|a, b| a.1.total_cmp(&b.1));
    near
}

/// A node to look at in A*, by the cost so far and what is left at best.
#[derive(PartialEq)]
struct Open {
    estimate: f32,
    node: usize,
}

impl Eq for Open {}

impl Ord for Open {
    fn cmp(&self, other: &Open) -> Ordering {
        other.estimate.total_cmp(&self.estimate).then(other.node.cmp(&self.node))
    }
}

impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Open) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A way for a hull from one point to another: the points to go through, the goal last;
/// `None` when there is none. The world alone is in the way; what moves is met walking.
pub(crate) fn route(lua: &Lua, from: Vector, to: Vector, hull: &Hull) -> Option<Vec<Vector>> {
    if reaches(lua, from, to, hull, &Solid::World) {
        return Some(vec![to]);
    }
    let nodes = nodes(lua);
    if nodes.is_empty() {
        return None;
    }
    let links = links(lua, hull);
    let starts: Vec<(usize, f32)> = nearest(&nodes, &links, from, hull)
        .into_iter()
        .filter(|&(i, _)| reaches(lua, from, nodes[i].pos, hull, &Solid::World))
        .take(ENDS)
        .collect();
    let ends: HashMap<usize, f32> = nearest(&nodes, &links, to, hull)
        .into_iter()
        .filter(|&(i, _)| reaches(lua, nodes[i].pos, to, hull, &Solid::World))
        .take(ENDS)
        .collect();
    if starts.is_empty() || ends.is_empty() {
        return None;
    }
    let mut cost = vec![f32::INFINITY; nodes.len()];
    let mut came: Vec<Option<usize>> = vec![None; nodes.len()];
    let mut open = BinaryHeap::new();
    for &(i, d) in &starts {
        cost[i] = d;
        open.push(Open { estimate: d + (nodes[i].pos - to).length(), node: i });
    }
    // the best way found to the goal: through which end node, and how long
    let mut best: Option<(usize, f32)> = None;
    while let Some(Open { estimate, node }) = open.pop() {
        if best.is_some_and(|(_, b)| estimate >= b) {
            break;
        }
        if let Some(&last) = ends.get(&node) {
            let total = cost[node] + last;
            if best.is_none_or(|(_, b)| total < b) {
                best = Some((node, total));
            }
        }
        for &(next, d) in &links[node] {
            let through = cost[node] + d;
            if through < cost[next] {
                cost[next] = through;
                came[next] = Some(node);
                open.push(Open { estimate: through + (nodes[next].pos - to).length(), node: next });
            }
        }
    }
    let (end, _) = best?;
    let mut way = vec![to];
    let mut at = Some(end);
    while let Some(node) = at {
        way.push(nodes[node].pos);
        at = came[node];
    }
    way.reverse();
    Some(way)
}

/// A point on the ground a hull reaches from `from`, from `min` to `max` away: a node's, or
/// a point along a way it walks. For wandering.
pub(crate) fn random_spot(lua: &Lua, from: Vector, min: f32, max: f32, hull: &Hull, roll: impl Fn(usize) -> usize) -> Option<Vector> {
    let nodes = nodes(lua);
    if !nodes.is_empty() {
        let links = links(lua, hull);
        let near: Vec<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].air == hull.air && !links[i].is_empty())
            .filter(|&i| (min..=max).contains(&(nodes[i].pos - from).length()))
            .collect();
        if !near.is_empty() {
            return Some(nodes[near[roll(near.len())]].pos);
        }
    }
    // no node: a few ways out, the first that walks far enough
    for attempt in 0..8 {
        let yaw = ((roll(360) + attempt * 45) % 360) as f32;
        let (s, c) = yaw.to_radians().sin_cos();
        let length = min + (max - min) * (roll(100) as f32 / 100.0);
        let spot = from + Vector::new(c, s, 0.0) * length;
        let spot = if hull.air { spot } else { floor_below(lua, spot + up(STEP)).unwrap_or(spot) };
        if reaches(lua, from, spot, hull, &Solid::World) {
            return Some(spot);
        }
    }
    None
}
