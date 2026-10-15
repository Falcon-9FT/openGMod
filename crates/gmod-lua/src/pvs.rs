//! What each player's client is sent of the entities: what the player may see.
//!
//! As the engine does, the server sends a player only the entities in the clusters of the map
//! its eyes may see - the PVS ([`gmod_bsp::Visibility`]) - from where it looks, or from its view
//! entity (`SetViewEntity`); `SetupPlayerVisibility` may add the PVS of other places
//! (`AddOriginToPVS`), as cameras and portals do. An entity is in sight when its box touches a
//! cluster of the set; one across more clusters than the engine follows, or in none, is in sight
//! everywhere. The world is sent always, and so are players: ours is the choice to keep every
//! player in every client's list, as scoreboards count on it. A view model goes to its owner
//! alone; what is parented goes with its parent; a SENT says for itself with
//! `ENT:UpdateTransmitState` (`TRANSMIT_ALWAYS`, `TRANSMIT_NEVER`, `TRANSMIT_PVS`), asked once and
//! again when it is flagged `EFL_FORCE_CHECK_TRANSMIT`; and `SetPreventTransmit` keeps an entity
//! from players. A map compiled without visibility sends everything to everyone.
//!
//! What is behind a shut area portal is not sent either, whatever the PVS says: an entity goes
//! to a player when one of the areas its box touches is joined to the player's through the
//! portals open. A `func_areaportal` opens and shuts its portal (`portalnumber`) by its inputs
//! (`Open`, `Close`, `Toggle`; open at first unless `StartOpen` is 0), or with the door it names
//! (`target`): open while the door is not shut. A `func_areaportalwindow` is always open.
//!
//! A client keeps what it was sent and is out of sight now, dormant ([`crate::snapshot`]):
//! `IsDormant`, not drawn, not thinking; `NotifyShouldTransmit` tells it when one goes and comes.
//! The same sets say where what is heard is heard: `RecipientFilter:AddPVS`, `AddPAS`,
//! `net.SendPVS`, `net.SendPAS` ([`players_seeing`], [`players_hearing`]).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gmod_bsp::{Visibility, row_has};
use mlua::{Lua, MultiValue, Value};

use crate::entity::{self, EntityHandle};
use crate::host;
use crate::vector::Vector;

const TRANSMIT_ALWAYS: i64 = 0;
const TRANSMIT_NEVER: i64 = 1;
/// `EFL_FORCE_CHECK_TRANSMIT`: the SENT is asked again how it is sent.
pub(crate) const EFL_FORCE_CHECK_TRANSMIT: i64 = 128;

/// Where an entity's box was, the clusters it touched and whether that is all of them, and
/// the areas it touched.
type Placed = ((Vector, Vector), Vec<usize>, bool, Vec<u16>);

/// What a player sees: the clusters (`None`: all) and the areas joined to its own (`None`: all).
#[derive(Debug, Clone, Default)]
struct Sight {
    clusters: Option<Vec<u8>>,
    areas: Option<Vec<bool>>,
}

/// What the server keeps to tell what each player is sent.
#[derive(Default)]
pub(crate) struct Transmit {
    /// The map's clusters, what each sees and hears; none: everything sees everything.
    pub visibility: RefCell<Option<Arc<Visibility>>>,
    /// The players each entity is kept from (`SetPreventTransmit`).
    prevented: RefCell<HashMap<EntityHandle, Vec<EntityHandle>>>,
    /// What SENTs' `UpdateTransmitState` answered; -1 for one that has none.
    states: RefCell<HashMap<EntityHandle, i64>>,
    /// The clusters of each entity's box, as it was when they were found: the box, the
    /// clusters, and whether that is all of them.
    clusters: RefCell<HashMap<EntityHandle, Placed>>,
    /// What a player sees, while `SetupPlayerVisibility` adds to it.
    adding: RefCell<Option<Sight>>,
    /// Whether each `func_areaportal` was opened or shut by its inputs.
    portals: RefCell<HashMap<EntityHandle, bool>>,
}

/// How an entity is sent.
#[derive(Debug, Clone, PartialEq)]
enum Rule {
    Always,
    Never,
    /// To this player alone.
    Owner(EntityHandle),
    /// Where these clusters are in sight and one of these areas is joined to the player's.
    Seen(Vec<usize>, Vec<u16>),
    /// As its parent is.
    With(EntityHandle),
}

/// The entities of a tick, each with how it is sent ([`crate::Runtime::transmission`]).
#[derive(Debug, Clone, Default)]
pub struct Transmission {
    entries: Vec<(EntityHandle, Rule)>,
    /// The keys of the area portals open.
    open: HashSet<u16>,
}

/// What a player is sent of a [`Transmission`]: the entities by index, and the others that are
/// there - index and serial number - which its client keeps dormant if it has them.
#[derive(Debug, Clone, Default)]
pub struct Sent {
    pub indices: std::collections::HashSet<u32>,
    pub held: Vec<(u32, u32)>,
}

/// The map's visibility, if it has some.
fn visibility(lua: &Lua) -> Option<Arc<Visibility>> {
    host(lua).transmit.visibility.borrow().clone()
}

fn array(v: Vector) -> [f32; 3] {
    [v.x, v.y, v.z]
}

/// The box of an entity in the world.
fn world_box(lua: &Lua, handle: EntityHandle) -> Option<(Vector, Vector)> {
    let (pos, angles) = host(lua).entities.transform(handle)?;
    let (mins, maxs) = host(lua).entities.get(handle).map(|e| (e.mins, e.maxs))?;
    let (lo, hi) = entity::rotated_bounds(mins, maxs, angles);
    Some((pos + lo, pos + hi))
}

/// The clusters an entity's box touches and whether that is all of them, and its areas.
fn clusters_of(lua: &Lua, vis: &Visibility, handle: EntityHandle) -> (Vec<usize>, bool, Vec<u16>) {
    let Some(bounds) = world_box(lua, handle) else { return (Vec::new(), true, Vec::new()) };
    if let Some((was, clusters, all, areas)) = host(lua).transmit.clusters.borrow().get(&handle)
        && *was == bounds
    {
        return (clusters.clone(), *all, areas.clone());
    }
    let mut clusters = Vec::new();
    let all = vis.clusters_in_box(array(bounds.0), array(bounds.1), &mut clusters);
    let mut areas = Vec::new();
    vis.areas_in_box(array(bounds.0), array(bounds.1), &mut areas);
    host(lua).transmit.clusters.borrow_mut().insert(handle, (bounds, clusters.clone(), all, areas.clone()));
    (clusters, all, areas)
}

/// The keys of the area portals open now.
fn open_portals(lua: &Lua) -> HashSet<u16> {
    let portals: Vec<(EntityHandle, String, Option<u16>, String, bool)> = {
        let entities = &host(lua).entities;
        entities
            .handles()
            .into_iter()
            .filter_map(|h| {
                let ent = entities.get(h)?;
                let class = ent.class.to_ascii_lowercase();
                if !class.starts_with("func_areaportal") || ent.removed {
                    return None;
                }
                let key = ent.keyvalue("portalnumber").and_then(|k| k.trim().parse::<u16>().ok());
                let target = ent.keyvalue("target").unwrap_or_default().to_string();
                let start_open = ent.keyvalue("StartOpen").is_none_or(|v| v.trim() != "0");
                Some((h, class, key, target, start_open))
            })
            .collect()
    };
    let states = host(lua).transmit.portals.borrow().clone();
    let mut open = HashSet::new();
    for (handle, class, key, target, start_open) in portals {
        let Some(key) = key else { continue };
        let door = (!target.is_empty()).then(|| crate::ents::named(lua, &target)).flatten();
        let is_open = match door.and_then(|d| crate::movers::openness(lua, d)) {
            // a door's portal opens as the door does, and shuts once it is shut
            Some(openness) => openness > 0.0,
            None => class == "func_areaportalwindow" || states.get(&handle).copied().unwrap_or(start_open),
        };
        if is_open {
            open.insert(key);
        }
    }
    open
}

/// An input of a `func_areaportal`: `Open`, `Close`, `Toggle`. Whether it took it.
pub(crate) fn input(lua: &Lua, target: EntityHandle, input: &str) -> bool {
    let Some((class, start_open)) =
        host(lua).entities.get(target).map(|e| (e.class.to_ascii_lowercase(), e.keyvalue("StartOpen").is_none_or(|v| v.trim() != "0")))
    else {
        return false;
    };
    if class != "func_areaportal" {
        return false;
    }
    let host = host(lua);
    let mut portals = host.transmit.portals.borrow_mut();
    let now = portals.get(&target).copied().unwrap_or(start_open);
    let next = match input.to_ascii_lowercase().as_str() {
        "open" => true,
        "close" => false,
        "toggle" => !now,
        _ => return false,
    };
    portals.insert(target, next);
    true
}

/// What a SENT's `UpdateTransmitState` says, asked once.
fn transmit_state(lua: &Lua, handle: EntityHandle) -> i64 {
    if let Some(&state) = host(lua).transmit.states.borrow().get(&handle) {
        return state;
    }
    let answer = match entity::push(lua, handle) {
        Ok(Value::UserData(ud)) => crate::ents::call_method(lua, &ud, "UpdateTransmitState", MultiValue::new()),
        _ => None,
    };
    let state = match answer.as_ref().and_then(|r| r.front()) {
        Some(Value::Integer(n)) => *n,
        Some(Value::Number(n)) => *n as i64,
        _ => -1,
    };
    host(lua).transmit.states.borrow_mut().insert(handle, state);
    state
}

/// A SENT flagged `EFL_FORCE_CHECK_TRANSMIT` is asked again.
pub(crate) fn recheck(lua: &Lua, handle: EntityHandle) {
    host(lua).transmit.states.borrow_mut().remove(&handle);
}

/// How each entity of the server is sent now; the server's own (`info_`, `logic_` ...) are not.
pub(crate) fn transmission(lua: &Lua) -> Transmission {
    let vis = visibility(lua);
    let handles = host(lua).entities.handles();
    {
        // what is gone is forgotten
        let host = host(lua);
        let transmit = &host.transmit;
        transmit.prevented.borrow_mut().retain(|h, _| host.entities.is_valid(*h));
        transmit.states.borrow_mut().retain(|h, _| host.entities.is_valid(*h));
        transmit.clusters.borrow_mut().retain(|h, _| host.entities.is_valid(*h));
        transmit.portals.borrow_mut().retain(|h, _| host.entities.is_valid(*h));
    }
    let mut entries = Vec::with_capacity(handles.len());
    for handle in handles {
        let Some(index) = handle.index else { continue };
        let Some((class, removed, player, scripted, parent, owner)) =
            host(lua).entities.get(handle).map(|e| (e.class.clone(), e.removed, e.player.is_some(), e.scripted, e.parent, e.owner))
        else {
            continue;
        };
        if removed || (index != 0 && crate::snapshot::server_only(&class)) {
            continue;
        }
        let state = if scripted { transmit_state(lua, handle) } else { -1 };
        let rule = if index == 0 || player || state == TRANSMIT_ALWAYS {
            Rule::Always
        } else if state == TRANSMIT_NEVER {
            Rule::Never
        } else if matches!(class.as_str(), "viewmodel" | "predicted_viewmodel") {
            Rule::Owner(owner)
        } else if host(lua).entities.is_valid(parent) {
            Rule::With(parent)
        } else {
            match &vis {
                Some(vis) => match clusters_of(lua, vis, handle) {
                    (clusters, true, areas) if !clusters.is_empty() => Rule::Seen(clusters, areas),
                    _ => Rule::Always,
                },
                None => Rule::Always,
            }
        };
        entries.push((handle, rule));
    }
    let open = if vis.is_some() { open_portals(lua) } else { HashSet::new() };
    Transmission { entries, open }
}

/// What is seen from a point: the clusters, and the areas joined to its own by the portals
/// open.
fn sight_from(vis: &Visibility, point: Vector, open: &HashSet<u16>) -> Sight {
    Sight {
        clusters: vis.cluster_at(array(point)).map(|c| vis.pvs(c).to_vec()),
        areas: vis.area_at(array(point)).map(|a| vis.connected_areas(a, &|key| open.contains(&key))),
    }
}

/// What a player sees: from its view entity or its eyes, with what `SetupPlayerVisibility` adds.
fn player_sight(lua: &Lua, vis: &Visibility, player: EntityHandle, open: &HashSet<u16>) -> Sight {
    let view = crate::player::state(lua, player, |_, p| p.view_entity).filter(|&v| host(lua).entities.is_valid(v));
    let from = match view {
        Some(v) => host(lua).entities.transform(v).map(|(p, _)| p),
        None => crate::player::eye(lua, player).map(|(e, _)| e),
    };
    let Some(from) = from else { return Sight::default() };
    *host(lua).transmit.adding.borrow_mut() = Some(sight_from(vis, from, open));
    let args = [entity::push(lua, player), entity::push(lua, view.unwrap_or(EntityHandle::NULL))];
    crate::call_hook(lua, "SetupPlayerVisibility", args.into_iter().flatten().collect());
    host(lua).transmit.adding.borrow_mut().take().unwrap_or_default()
}

/// `AddOriginToPVS`, in `SetupPlayerVisibility`: what is seen from there is seen too.
fn add_origin(lua: &Lua, origin: Vector) {
    let Some(vis) = visibility(lua) else { return };
    // from there, through the portals open as they are now
    let open = open_portals(lua);
    let more = sight_from(&vis, origin, &open);
    let host = host(lua);
    let mut adding = host.transmit.adding.borrow_mut();
    let Some(sight) = adding.as_mut() else { return };
    sight.clusters = match (sight.clusters.take(), more.clusters) {
        (Some(mut row), Some(other)) => {
            for (a, b) in row.iter_mut().zip(&other) {
                *a |= b;
            }
            Some(row)
        }
        // from no cluster, everything is seen
        _ => None,
    };
    sight.areas = match (sight.areas.take(), more.areas) {
        (Some(mut joined), Some(other)) => {
            for (a, b) in joined.iter_mut().zip(&other) {
                *a |= b;
            }
            Some(joined)
        }
        _ => None,
    };
}

impl Transmission {
    /// What a player is sent.
    pub(crate) fn for_player(&self, lua: &Lua, player: EntityHandle) -> Sent {
        let sight = visibility(lua).map(|vis| player_sight(lua, &vis, player, &self.open)).unwrap_or_default();
        let view = crate::player::state(lua, player, |_, p| p.view_entity).unwrap_or(EntityHandle::NULL);
        let prevented = host(lua).transmit.prevented.borrow().clone();
        let mut decided: HashMap<EntityHandle, bool> = HashMap::with_capacity(self.entries.len());
        let by_handle: HashMap<EntityHandle, &Rule> = self.entries.iter().map(|(h, r)| (*h, r)).collect();
        let own = |handle: EntityHandle, rule: &Rule| -> Option<bool> {
            Some(
                match rule {
                    Rule::Always => true,
                    Rule::Never => false,
                    Rule::Owner(owner) => *owner == player,
                    Rule::Seen(clusters, areas) => {
                        sight.clusters.as_ref().is_none_or(|row| clusters.iter().any(|&c| row_has(row, c)))
                            && (areas.is_empty()
                                || sight
                                    .areas
                                    .as_ref()
                                    .is_none_or(|joined| areas.iter().any(|&a| joined.get(usize::from(a)) == Some(&true))))
                    }
                    Rule::With(_) => return None,
                } || handle == view,
            )
        };
        for (handle, rule) in &self.entries {
            if let Some(sent) = own(*handle, rule) {
                decided.insert(*handle, sent);
            }
        }
        let mut sent = Sent::default();
        for (handle, _) in &self.entries {
            let mut yes = match decided.get(handle) {
                Some(&yes) => yes,
                None => {
                    // as the parent, all the way up; a parent the clients never get leaves it sent
                    let mut at = *handle;
                    let mut found = None;
                    for _ in 0..16 {
                        match by_handle.get(&at) {
                            Some(Rule::With(parent)) => at = *parent,
                            Some(_) => {
                                found = decided.get(&at).copied();
                                break;
                            }
                            None => break,
                        }
                    }
                    found.unwrap_or(true) || *handle == view
                }
            };
            if prevented.get(handle).is_some_and(|players| players.contains(&player)) {
                yes = false;
            }
            let index = handle.index.unwrap_or_default();
            if yes {
                sent.indices.insert(index);
            } else {
                sent.held.push((index, handle.serial));
            }
        }
        sent
    }
}

/// The players whose eyes are in a set of clusters (all of them for none).
fn players_in(lua: &Lua, vis: &Visibility, set: Option<&[u8]>) -> Vec<EntityHandle> {
    crate::player::all(lua)
        .into_iter()
        .filter(|&p| match set {
            None => true,
            Some(row) => crate::player::eye(lua, p).and_then(|(eye, _)| vis.cluster_at(array(eye))).is_none_or(|c| row_has(row, c)),
        })
        .collect()
}

/// The players that may see a point (`AddPVS`, `net.SendPVS`): everyone on a map without
/// visibility.
pub(crate) fn players_seeing(lua: &Lua, point: Vector) -> Vec<EntityHandle> {
    match visibility(lua) {
        Some(vis) => {
            let set = vis.cluster_at(array(point)).map(|c| vis.pvs(c).to_vec());
            players_in(lua, &vis, set.as_deref())
        }
        None => crate::player::all(lua),
    }
}

/// The players that may hear a point (`AddPAS`, `net.SendPAS`, sounds).
pub(crate) fn players_hearing(lua: &Lua, point: Vector) -> Vec<EntityHandle> {
    match visibility(lua) {
        Some(vis) => {
            let set = vis.cluster_at(array(point)).map(|c| vis.pas(c).to_vec());
            players_in(lua, &vis, set.as_deref())
        }
        None => crate::player::all(lua),
    }
}

/// The players who hear a sound: those in the clusters heard from where it is; `None` for
/// everyone - a sound of no place, of the world, or that carries everywhere (level 0), and
/// what stops or changes sounds.
pub(crate) fn sound_hearers(lua: &Lua, event: &crate::sound::SoundEvent) -> Option<Vec<EntityHandle>> {
    let crate::sound::SoundEvent::Play(play) = event else { return None };
    if play.level <= 0.0 {
        return None;
    }
    let at = match (play.position, play.entity) {
        (Some(p), _) => p,
        (None, Some(e)) if e != EntityHandle::WORLD => host(lua).entities.transform(e).map(|(p, _)| p)?,
        _ => return None,
    };
    visibility(lua)?;
    Some(players_hearing(lua, at))
}

/// The server's `AddOriginToPVS`.
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().raw_set(
        "AddOriginToPVS",
        lua.create_function(|lua, values: MultiValue| {
            let a = crate::args::Args::new(lua, values, "AddOriginToPVS");
            add_origin(lua, a.userdata::<Vector>(1, "Vector")?);
            Ok(())
        })?,
    )
}

pub(crate) fn install_methods(def: &entity::Def) -> mlua::Result<()> {
    def("SetPreventTransmit", |a| {
        let handle = entity::this(a, 1)?;
        let players = crate::recipients::players_of(a.lua, a.get(2));
        let stop = a.boolean(3);
        let host = host(a.lua);
        let mut prevented = host.transmit.prevented.borrow_mut();
        let list = prevented.entry(handle).or_default();
        list.retain(|p| !players.contains(p));
        if stop {
            list.extend(players);
        }
        Ok(Value::Nil)
    })?;
    // whether a point or an entity is in what this entity sees: a player from their eyes,
    // anything else from where it is; everything on a map without visibility
    def("TestPVS", |a| {
        let handle = entity::this(a, 1)?;
        let Some(vis) = visibility(a.lua) else { return Ok(Value::Boolean(true)) };
        let from = match crate::player::eye(a.lua, handle) {
            Some((eye, _)) => Some(eye),
            None => host(a.lua).entities.transform(handle).map(|(pos, _)| pos),
        };
        let Some(cluster) = from.and_then(|from| vis.cluster_at(array(from))) else { return Ok(Value::Boolean(true)) };
        let row = vis.pvs(cluster).to_vec();
        let seen = match a.get(2) {
            Some(Value::UserData(ud)) if ud.is::<Vector>() => {
                let point = *ud.borrow::<Vector>()?;
                vis.cluster_at(array(point)).is_none_or(|c| row_has(&row, c))
            }
            other => match entity::handle_of(other) {
                Some(target) => {
                    let (clusters, all, _) = clusters_of(a.lua, &vis, target);
                    all || clusters.iter().any(|&c| row_has(&row, c))
                }
                None => false,
            },
        };
        Ok(Value::Boolean(seen))
    })?;
    def("IsDormant", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(host(a.lua).entities.get(handle).is_some_and(|e| e.dormant)))
    })
}
