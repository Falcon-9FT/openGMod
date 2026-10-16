//! What openGMOD's server and its clients say to each other, over a [`gmod_net`] connection.
//!
//! A client connects saying [`Hello`]; the server lets it in with a [`Welcome`] - the map, the
//! gamemode - and then lists the files the client is to have ([`FromServer::Files`]): the
//! client Lua (`AddCSLuaFile`), and what `resource.AddFile` named with the map itself. The
//! client asks for those it lacks or has otherwise ([`ToServer::Want`]), the server sends them
//! in pieces, the client loads the map and its Lua and says it is [`ToServer::Ready`]; then the
//! player joins the game.
//!
//! In the game the server sends every tick what the client sees of the entities as a
//! [`SnapshotDelta`] - unreliable, against the newest snapshot the client said it has, so that
//! what did not change costs nothing - and what happened (the chat, `net` messages, sounds,
//! what it asks of the client) reliably. The client sends its commands of the last frames,
//! unreliable and each a few times over, and what it says and asks reliably.
//!
//! Every message is serialized with postcard, and compressed when that makes it smaller.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use gmod_lua::net::Bits;
use gmod_lua::sound::SoundEvent;
use gmod_lua::{ChatMessage, EntityHandle, EntityState, Snapshot, ToClient, UserCmd};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The version of these messages: a client and a server of different ones do not play.
pub const VERSION: u32 = 4;

/// The port servers listen on unless told otherwise.
pub const DEFAULT_PORT: u16 = 27015;

/// What a client says as it connects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u32,
    /// The player's name.
    pub name: String,
}

/// A file the server has, for a client to compare with its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub crc: u32,
}

impl FileInfo {
    pub fn of(path: &str, data: &[u8]) -> FileInfo {
        FileInfo { path: path.to_string(), size: data.len() as u64, crc: gmod_fs::crc32::crc32(data) }
    }
}

/// What the server answers a client it lets in: it goes in one packet
/// ([`gmod_net::MAX_HELLO`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    /// The server's name (`hostname`).
    pub server: String,
    pub map: String,
    pub gamemode: String,
    pub max_players: u32,
    /// `sv_allowcslua`: its players may run Lua of their own - their addons' too.
    pub allow_cs_lua: bool,
    /// `sv_loadingurl`: the page its players see while they load.
    pub loading_url: String,
    /// The seconds of its tick (`-tickrate`): its players' clients tick as it does.
    pub tick_interval: f64,
}

/// What a server says of itself to those who ask ([`gmod_net::Query`]): a line of the menu's
/// server browser.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// `hostname`.
    pub name: String,
    pub map: String,
    /// The gamemode's folder, and what it calls itself.
    pub gamemode: String,
    pub title: String,
    pub players: u32,
    pub max_players: u32,
    pub bots: u32,
    /// The game's protocol: a client of another cannot join ([`VERSION`]).
    pub version: u32,
    /// A number the server picked as it started: one server, though it answers at several
    /// addresses (this computer's own, the local network's), is listed once.
    pub instance: u64,
    /// Its players - name, frags, seconds played - as many as fit an answer.
    pub list: Vec<(String, i64, f32)>,
}

impl ServerInfo {
    /// Serialized to fit an answer ([`gmod_net::MAX_INFO`]): players left off the list until
    /// it does.
    pub fn encode(&self) -> Vec<u8> {
        let mut info = self.clone();
        loop {
            let bytes = postcard::to_allocvec(&info).unwrap_or_default();
            if bytes.len() <= gmod_net::MAX_INFO || info.list.is_empty() {
                return bytes;
            }
            info.list.pop();
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<ServerInfo> {
        postcard::from_bytes(bytes).ok()
    }
}

/// What a client sends its server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToServer {
    /// The files of the welcome it does not have as the server has them.
    Want(Vec<String>),
    /// It has loaded the map and its Lua: its player joins.
    Ready,
    /// Its latest commands, oldest first, the last numbered `last`; and the newest snapshot it
    /// has (0 for none).
    /// And the time of the game it showed then, which its shots go back to (lag
    /// compensation).
    Commands { last: u32, commands: Vec<UserCmd>, ack: u32, view: f64 },
    /// A console line for the server, run as the player.
    Command(String),
    /// A value of its user info (`cl_playermodel`).
    UserInfo(String, String),
    /// A `net` message.
    Net { name: String, data: Bits },
    /// A key or button of the game's went down (true) or up (`PlayerButtonDown`).
    Button(i32, bool),
}

/// What a server sends a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FromServer {
    /// The files a client is to have: the client Lua, mounted over the client's own files for
    /// the game; and the content, kept in openGMOD's folder behind the game's own.
    Files {
        lua: Vec<FileInfo>,
        content: Vec<FileInfo>,
    },
    /// A piece of a file it wanted: `size` bytes in all, this from `offset`.
    File {
        path: String,
        offset: u64,
        size: u64,
        data: Vec<u8>,
    },
    /// Every file it wanted has been sent.
    FilesSent,
    /// The player joined: its entity.
    Joined {
        player: EntityHandle,
    },
    Snapshot(SnapshotDelta),
    Chat(ChatMessage),
    Net {
        name: String,
        data: Bits,
    },
    Sounds(Vec<SoundEvent>),
    Asked(ToClient),
}

/// How a message starts: as it is, or compressed.
const RAW: u8 = 0;
const DEFLATED: u8 = 1;
/// Messages shorter than this are not worth compressing.
const COMPRESS_FROM: usize = 128;
/// The most a message may grow to when it is inflated.
const MAX_MESSAGE: u64 = 64 << 20;

/// A message as bytes.
pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    let raw = postcard::to_allocvec(message).unwrap_or_default();
    if raw.len() >= COMPRESS_FROM {
        let deflated = gmod_fs::deflate::deflate(&raw);
        if deflated.len() < raw.len() {
            let mut out = Vec::with_capacity(deflated.len() + 1);
            out.push(DEFLATED);
            out.extend_from_slice(&deflated);
            return out;
        }
    }
    let mut out = Vec::with_capacity(raw.len() + 1);
    out.push(RAW);
    out.extend_from_slice(&raw);
    out
}

/// A message from bytes; none when they are not one.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    let (&kind, rest) = bytes.split_first()?;
    match kind {
        RAW => postcard::from_bytes(rest).ok(),
        DEFLATED => postcard::from_bytes(&gmod_fs::inflate::inflate(rest, rest.len() * 4, MAX_MESSAGE).ok()?).ok(),
        _ => None,
    }
}

/// The entities of a snapshot, each serialized, by index.
pub type Encoded = BTreeMap<u32, Arc<[u8]>>;

/// Where an [`Encoded`] keeps the entities its client is to keep dormant: past every index.
pub const DORMANT: u32 = u32::MAX;

/// Serializes the entities of a snapshot, for the clients' deltas.
pub fn encode_entities(snapshot: &Snapshot) -> Encoded {
    snapshot.entities.iter().map(|e| (e.index, Arc::from(postcard::to_allocvec(e).unwrap_or_default()))).collect()
}

/// What of the entities one client is sent: those of `indices`, and the list of those it is to
/// keep dormant - as one more entry, so that an unchanged list costs nothing in a delta.
pub fn for_player(all: &Encoded, indices: &HashSet<u32>, dormant: &[(u32, u32)]) -> Encoded {
    let mut mine: Encoded = all.iter().filter(|(i, _)| indices.contains(i)).map(|(i, b)| (*i, b.clone())).collect();
    if !dormant.is_empty() {
        mine.insert(DORMANT, Arc::from(postcard::to_allocvec(dormant).unwrap_or_default()));
    }
    mine
}

/// A snapshot as what changed since one the client has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDelta {
    /// Its number, counting from 1.
    pub seq: u32,
    /// The snapshot it changes, 0 for none: then it has every entity.
    pub baseline: u32,
    /// The server's `CurTime`.
    pub time: f64,
    /// The last of the client's commands the server had taken: the client moves its player
    /// on from this snapshot by those after it.
    pub command: u32,
    /// The entities of the baseline that are gone.
    pub removed: Vec<u32>,
    /// The entities new or changed, serialized: as they are, or (`true`) as the bytes that
    /// differ from the baseline's, which then had as many - mostly zeros, which compress.
    pub changed: Vec<(u32, bool, Vec<u8>)>,
}

/// How many snapshots each side keeps to make deltas against.
const HISTORY: usize = 64;

/// The snapshots a server sent one client, for deltas against what the client has.
#[derive(Default)]
pub struct SnapshotSender {
    history: VecDeque<(u32, Arc<Encoded>)>,
    last: u32,
    acked: u32,
}

impl SnapshotSender {
    /// The client has snapshot `seq`.
    pub fn ack(&mut self, seq: u32) {
        if seq > self.acked && seq <= self.last {
            self.acked = seq;
        }
    }

    /// The next snapshot for the client, made after its command `command`: what changed since
    /// the newest it has.
    pub fn delta(&mut self, entities: &Arc<Encoded>, time: f64, command: u32) -> SnapshotDelta {
        self.last += 1;
        let seq = self.last;
        let base = self.history.iter().find(|(s, _)| *s == self.acked).map(|(_, e)| e.clone());
        let empty = Encoded::new();
        let old = base.as_deref().unwrap_or(&empty);
        let removed = old.keys().filter(|index| !entities.contains_key(index)).copied().collect();
        let changed = entities
            .iter()
            .filter_map(|(&index, bytes)| match old.get(&index) {
                Some(before) if Arc::ptr_eq(before, bytes) || before == bytes => None,
                Some(before) if before.len() == bytes.len() => {
                    Some((index, true, before.iter().zip(bytes.iter()).map(|(a, b)| a ^ b).collect()))
                }
                _ => Some((index, false, bytes.to_vec())),
            })
            .collect();
        self.history.push_back((seq, entities.clone()));
        // what is older than the acknowledged one is no use any more
        while self.history.len() > HISTORY || self.history.front().is_some_and(|(s, _)| *s < self.acked) {
            self.history.pop_front();
        }
        SnapshotDelta { seq, baseline: if base.is_some() { self.acked } else { 0 }, time, command, removed, changed }
    }
}

/// The snapshots a client received, to apply deltas to.
#[derive(Default)]
pub struct SnapshotReceiver {
    history: VecDeque<(u32, Encoded)>,
    /// The entities as last decoded, by index, with the bytes they came from.
    decoded: HashMap<u32, (Arc<[u8]>, EntityState)>,
    /// The newest snapshot taken, for the server to make deltas against.
    pub newest: u32,
}

impl SnapshotReceiver {
    /// The snapshot a delta makes, if it is newer than the last and its baseline is here.
    pub fn receive(&mut self, delta: SnapshotDelta) -> Option<Snapshot> {
        if delta.seq <= self.newest {
            return None;
        }
        let mut entities = match delta.baseline {
            0 => Encoded::new(),
            seq => self.history.iter().find(|(s, _)| *s == seq)?.1.clone(),
        };
        for index in &delta.removed {
            entities.remove(index);
        }
        for (index, diff, bytes) in delta.changed {
            let bytes: Arc<[u8]> = if diff {
                let before = entities.get(&index).filter(|b| b.len() == bytes.len())?;
                before.iter().zip(&bytes).map(|(a, b)| a ^ b).collect()
            } else {
                bytes.into()
            };
            entities.insert(index, bytes);
        }
        let mut states = Vec::with_capacity(entities.len());
        let mut decoded = HashMap::with_capacity(entities.len());
        let dormant = match entities.get(&DORMANT) {
            Some(bytes) => postcard::from_bytes::<Vec<(u32, u32)>>(bytes).ok()?,
            None => Vec::new(),
        };
        for (&index, bytes) in entities.iter().filter(|(i, _)| **i != DORMANT) {
            let state = match self.decoded.remove(&index) {
                Some((before, state)) if Arc::ptr_eq(&before, bytes) || before == *bytes => state,
                _ => postcard::from_bytes::<EntityState>(bytes).ok().filter(|s| s.index == index)?,
            };
            states.push(state.clone());
            decoded.insert(index, (bytes.clone(), state));
        }
        self.decoded = decoded;
        self.history.push_back((delta.seq, entities));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        self.newest = delta.seq;
        Some(Snapshot { time: delta.time, entities: states, dormant })
    }
}

/// Puts the messages a client sends into what the server's `set_input` takes: the newest
/// command, with an impulse from any of those not taken before (a key pressed once is not
/// lost with the packet that had it) and the mouse wheel and movement of all of them.
pub fn merge_commands(taken: u32, last: u32, commands: &[UserCmd]) -> Option<UserCmd> {
    let newest = *commands.last()?;
    let new_count = last.saturating_sub(taken).min(commands.len() as u32) as usize;
    if new_count == 0 {
        return None;
    }
    let new = &commands[commands.len() - new_count..];
    let mut merged = newest;
    merged.impulse = new.iter().rev().map(|c| c.impulse).find(|&i| i != 0).unwrap_or(0);
    merged.mouse_wheel = new.iter().map(|c| c.mouse_wheel).sum();
    merged.mouse_x = new.iter().map(|c| c.mouse_x).sum();
    merged.mouse_y = new.iter().map(|c| c.mouse_y).sum();
    Some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_and_big_ones_shrink() {
        let small = ToServer::Command("say hi".into());
        assert_eq!(decode::<ToServer>(&encode(&small)), Some(small));
        let big = FromServer::File { path: "lua/a.lua".into(), offset: 0, size: 10_000, data: vec![b'a'; 10_000] };
        let bytes = encode(&big);
        assert!(bytes.len() < 1000 && bytes[0] == DEFLATED, "{}", bytes.len());
        assert_eq!(decode::<FromServer>(&bytes), Some(big));
        assert_eq!(decode::<FromServer>(&[7, 1, 2]), None);
        assert_eq!(decode::<FromServer>(&[]), None);
        assert_eq!(decode::<FromServer>(&[DEFLATED, 0xff, 0xff]), None);
    }

    fn state(index: u32, x: f32) -> EntityState {
        let mut ent = gmod_lua::entity::Ent::new("prop_physics", gmod_lua::entity::Kind::Entity, index, 0.0);
        ent.local_pos = gmod_lua::vector::Vector::new(x, 0.0, 0.0);
        ent.model = "models/props_c17/oildrum001.mdl".into();
        EntityState { index, serial: 1, ent, vars: Default::default() }
    }

    fn snapshot(time: f64, entities: Vec<EntityState>) -> Snapshot {
        Snapshot { time, entities, dormant: Vec::new() }
    }

    fn positions(s: &Snapshot) -> Vec<(u32, f32)> {
        s.entities.iter().map(|e| (e.index, e.ent.local_pos.x)).collect()
    }

    #[test]
    fn deltas_carry_only_what_changed_since_what_the_client_has() {
        let mut sender = SnapshotSender::default();
        let mut receiver = SnapshotReceiver::default();
        // the first has everything
        let first = snapshot(1.0, (1..=50).map(|i| state(i, i as f32)).collect());
        let delta = sender.delta(&Arc::new(encode_entities(&first)), first.time, 0);
        assert_eq!((delta.baseline, delta.changed.len()), (0, 50));
        let got = receiver.receive(delta).unwrap();
        assert_eq!(positions(&got), positions(&first));
        sender.ack(receiver.newest);

        // one moves, one goes, one comes
        let mut second = first.clone();
        second.time = 1.1;
        second.entities[4].ent.local_pos.x = 99.0;
        second.entities.remove(9);
        second.entities.push(state(60, 60.0));
        let delta = sender.delta(&Arc::new(encode_entities(&second)), second.time, 0);
        assert_eq!(delta.baseline, 1);
        assert_eq!(delta.removed, [10]);
        assert_eq!(delta.changed.iter().map(|(i, diff, _)| (*i, *diff)).collect::<Vec<_>>(), [(5, true), (60, false)]);
        let size = encode(&FromServer::Snapshot(delta.clone())).len();
        assert!(size < 200, "{size} bytes");
        let got = receiver.receive(delta.clone()).unwrap();
        assert_eq!(positions(&got), positions(&second));
        assert!((got.time - 1.1).abs() < 1e-9);
        // the same again, or an older one, is not taken
        assert!(receiver.receive(delta).is_none());

        // lost on the way: the next is against what the client still has
        let mut third = second.clone();
        third.entities[0].ent.local_pos.x = -1.0;
        let lost = sender.delta(&Arc::new(encode_entities(&third)), 1.2, 0);
        let mut fourth = third.clone();
        fourth.entities[1].ent.local_pos.x = -2.0;
        let delta = sender.delta(&Arc::new(encode_entities(&fourth)), 1.3, 0);
        assert_eq!((lost.baseline, delta.baseline), (1, 1), "the client never said it had 2");
        let got = receiver.receive(delta).unwrap();
        assert_eq!(positions(&got), positions(&fourth));
        // a delta against a snapshot the client does not have makes nothing
        let orphan = SnapshotDelta { seq: 99, baseline: 77, time: 2.0, command: 0, removed: Vec::new(), changed: Vec::new() };
        assert!(receiver.receive(orphan).is_none());
    }

    #[test]
    fn each_client_gets_what_it_sees_and_keeps_the_rest_dormant() {
        let mut sender = SnapshotSender::default();
        let mut receiver = SnapshotReceiver::default();
        let all = encode_entities(&snapshot(1.0, (1..=10).map(|i| state(i, i as f32)).collect()));
        // it sees the first five; it has 6 and 7 from before
        let seen: HashSet<u32> = (1..=5).collect();
        let mine = for_player(&all, &seen, &[(6, 1), (7, 1)]);
        let got = receiver.receive(sender.delta(&Arc::new(mine.clone()), 1.0, 0)).unwrap();
        assert_eq!(got.entities.iter().map(|e| e.index).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert_eq!(got.dormant, [(6, 1), (7, 1)]);
        sender.ack(receiver.newest);
        // the same again: the list costs nothing
        let delta = sender.delta(&Arc::new(mine), 1.1, 0);
        assert!(delta.changed.is_empty() && delta.removed.is_empty());
        assert_eq!(receiver.receive(delta).unwrap().dormant, [(6, 1), (7, 1)]);
        sender.ack(receiver.newest);
        // all in sight: none dormant
        let seen: HashSet<u32> = (1..=10).collect();
        let delta = sender.delta(&Arc::new(for_player(&all, &seen, &[])), 1.2, 0);
        assert!(delta.removed.contains(&DORMANT));
        let got = receiver.receive(delta).unwrap();
        assert!(got.dormant.is_empty() && got.entities.len() == 10);
    }

    #[test]
    fn commands_lost_on_the_way_still_count() {
        let cmd = |impulse: u8, wheel: i32| UserCmd { impulse, mouse_wheel: wheel, mouse_x: 1, ..UserCmd::default() };
        // the server took up to 7; 8 had the flashlight, 9 none
        let merged = merge_commands(7, 9, &[cmd(0, 0), cmd(100, 1), cmd(0, 1)]).unwrap();
        assert_eq!((merged.impulse, merged.mouse_wheel, merged.mouse_x), (100, 2, 2));
        assert_eq!(merge_commands(9, 9, &[cmd(0, 0)]), None);
        assert_eq!(merge_commands(0, 3, &[]), None);
    }
}
