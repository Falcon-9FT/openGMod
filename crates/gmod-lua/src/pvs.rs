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
//! A client keeps what it was sent and is out of sight now, dormant ([`crate::snapshot`]):
//! `IsDormant`, not drawn, not thinking; `NotifyShouldTransmit` tells it when one goes and comes.
//! The same sets say where what is heard is heard: `RecipientFilter:AddPVS`, `AddPAS`,
//! `net.SendPVS`, `net.SendPAS` ([`players_seeing`], [`players_hearing`]).

use std::cell::RefCell;
use std::collections::HashMap;
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

/// Where an entity's box was, the clusters it touched, and whether that is all of them.
type Placed = ((Vector, Vector), Vec<usize>, bool);

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
    /// The set of clusters a player sees, while `SetupPlayerVisibility` adds to it; `Some(None)`
    /// when it is everything.
    adding: RefCell<Option<Option<Vec<u8>>>>,
}

/// How an entity is sent.
#[derive(Debug, Clone, PartialEq)]
enum Rule {
    Always,
    Never,
    /// To this player alone.
    Owner(EntityHandle),
    /// Where these clusters are in sight.
    Seen(Vec<usize>),
    /// As its parent is.
    With(EntityHandle),
}

/// The entities of a tick, each with how it is sent ([`crate::Runtime::transmission`]).
#[derive(Debug, Clone, Default)]
pub struct Transmission {
    entries: Vec<(EntityHandle, Rule)>,
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

/// The clusters an entity's box touches, and whether that is all of them.
fn clusters_of(lua: &Lua, vis: &Visibility, handle: EntityHandle) -> (Vec<usize>, bool) {
    let Some(bounds) = world_box(lua, handle) else { return (Vec::new(), true) };
    if let Some((was, clusters, all)) = host(lua).transmit.clusters.borrow().get(&handle)
        && *was == bounds
    {
        return (clusters.clone(), *all);
    }
    let mut clusters = Vec::new();
    let all = vis.clusters_in_box(array(bounds.0), array(bounds.1), &mut clusters);
    host(lua).transmit.clusters.borrow_mut().insert(handle, (bounds, clusters.clone(), all));
    (clusters, all)
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
                    (clusters, true) if !clusters.is_empty() => Rule::Seen(clusters),
                    _ => Rule::Always,
                },
                None => Rule::Always,
            }
        };
        entries.push((handle, rule));
    }
    Transmission { entries }
}

/// The clusters a player sees: from its view entity or its eyes, with what
/// `SetupPlayerVisibility` adds; `None` for all of them.
fn player_set(lua: &Lua, vis: &Visibility, player: EntityHandle) -> Option<Vec<u8>> {
    let view = crate::player::state(lua, player, |_, p| p.view_entity).filter(|&v| host(lua).entities.is_valid(v));
    let from = match view {
        Some(v) => host(lua).entities.transform(v).map(|(p, _)| p),
        None => crate::player::eye(lua, player).map(|(e, _)| e),
    }?;
    let set = vis.cluster_at(array(from)).map(|c| vis.pvs(c).to_vec());
    *host(lua).transmit.adding.borrow_mut() = Some(set);
    let args = [entity::push(lua, player), entity::push(lua, view.unwrap_or(EntityHandle::NULL))];
    crate::call_hook(lua, "SetupPlayerVisibility", args.into_iter().flatten().collect());
    host(lua).transmit.adding.borrow_mut().take().flatten()
}

/// `AddOriginToPVS`, in `SetupPlayerVisibility`: what is seen from there is seen too.
fn add_origin(lua: &Lua, origin: Vector) {
    let Some(vis) = visibility(lua) else { return };
    let host = host(lua);
    let mut adding = host.transmit.adding.borrow_mut();
    let Some(set) = adding.as_mut() else { return };
    match (set.as_mut(), vis.cluster_at(array(origin))) {
        (Some(row), Some(cluster)) => {
            for (a, b) in row.iter_mut().zip(vis.pvs(cluster)) {
                *a |= b;
            }
        }
        // from no cluster, everything is seen
        (Some(_), None) => *set = None,
        (None, _) => {}
    }
}

impl Transmission {
    /// What a player is sent.
    pub(crate) fn for_player(&self, lua: &Lua, player: EntityHandle) -> Sent {
        let set = visibility(lua).and_then(|vis| player_set(lua, &vis, player));
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
                    Rule::Seen(clusters) => set.as_ref().is_none_or(|row| clusters.iter().any(|&c| row_has(row, c))),
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
    def("IsDormant", |a| {
        let handle = entity::this(a, 1)?;
        Ok(Value::Boolean(host(a.lua).entities.get(handle).is_some_and(|e| e.dormant)))
    })
}
