//! The server of a game: the Lua of the server running the map, with the player at this
//! computer (a single-player game, or the host of a listen server) and those who connect over
//! the network ([`proto`](crate::proto)). A dedicated server has only the latter.
//!
//! Each frame the server reads what its clients sent (their commands, what they say and
//! ask), runs the game, and sends each what is theirs: a snapshot of the entities when a tick
//! has passed, the chat said to them, the `net` messages for them, the sounds, what the
//! server's Lua asks of their clients. What is for the host's client the caller takes
//! ([`ForHost`]): that client is in the same program and sees the server's entities directly.

use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gmod_fs::Vfs;
use gmod_lua::net::NetMessage;
use gmod_lua::sound::SoundEvent;
use gmod_lua::{ChatMessage, EntityHandle, Map, Output, PlayerInfo, Realm, Runtime, ToClient};
use gmod_net::{ClientId, Identity, PublicKey, ServerEvent};

use crate::keys;
use crate::proto::{self, FileInfo, FromServer, Hello, ServerInfo, SnapshotSender, ToServer, Welcome};

/// How much of a file goes in one message.
const FILE_PIECE: usize = 1 << 20;
/// How often a server tells its master servers it is here: they forget it after five minutes.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(60);

/// A Lua state of the game on the game's files, printing to the terminal and keeping its data
/// in openGMOD's folder.
pub fn runtime(realm: Realm, vfs: Arc<Vfs>) -> Result<Runtime, String> {
    let runtime = Runtime::new(realm, vfs).map_err(|e| format!("LuaJIT could not be started: {e}"))?;
    runtime.set_output(|output| match output {
        Output::Text(text) => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        Output::Error(error) => eprintln!("{error}"),
    });
    if let Some(dir) = gmod_fs::UserDir::default_root().and_then(|root| gmod_fs::UserDir::open(root, None).ok()) {
        runtime.set_data_dir(dir);
    }
    Ok(runtime)
}

/// What the server's frame left for the host's client.
#[derive(Debug, Default)]
pub struct ForHost {
    pub chat: Vec<ChatMessage>,
    pub net: Vec<NetMessage>,
    pub asked: Vec<ToClient>,
    pub sounds: Vec<SoundEvent>,
}

/// A client connected over the network.
struct Remote {
    id: ClientId,
    addr: SocketAddr,
    name: String,
    /// Who the player is: their key, proven as they connected.
    key: PublicKey,
    /// Its player, once it has loaded and joined.
    player: Option<EntityHandle>,
    snapshots: SnapshotSender,
    /// The entities it was sent, by index, with their serial numbers, while the server has
    /// them: those out of sight it keeps dormant.
    known: HashMap<u32, u32>,
    /// The number of the last command taken.
    command: u32,
}

/// The files clients are to have, worked out when the first connects (and again when the
/// server's Lua sent more).
struct Files {
    lua: Vec<FileInfo>,
    content: Vec<FileInfo>,
    /// How many files of each the server's Lua named then.
    named: (usize, usize),
}

pub struct Server {
    runtime: Runtime,
    vfs: Arc<Vfs>,
    map: String,
    gamemode: String,
    /// `hostname`.
    pub name: String,
    net: Option<gmod_net::Server>,
    remotes: Vec<Remote>,
    host: Option<EntityHandle>,
    /// The key of the player at this computer, who plays with it here already.
    host_key: Option<PublicKey>,
    files: Option<Files>,
    /// The tick of the last snapshot sent.
    last_tick: i64,
    /// When it last told the network what it is ([`ServerInfo`]), and what its gamemode is
    /// called.
    told: Option<Instant>,
    title: Option<String>,
    /// What tells this server from others that answer at the same address ([`ServerInfo`]).
    instance: u64,
    /// The master servers it tells it is here, and when it last did.
    masters: Vec<SocketAddr>,
    announced: Option<Instant>,
    started: Instant,
}

impl Server {
    /// Starts the Lua of the server on the map, for `max_players` (1: a single-player game).
    pub fn start(vfs: Arc<Vfs>, map: &Map, gamemode: &str, max_players: u32, dedicated: bool) -> Result<Server, String> {
        let runtime = runtime(Realm::Server, vfs.clone())?;
        runtime.set_max_players(max_players);
        runtime.set_dedicated(dedicated);
        runtime.start(gamemode);
        runtime.spawn_map(map);
        Ok(Server {
            runtime,
            vfs,
            map: map.name.clone(),
            gamemode: gamemode.to_string(),
            name: "openGMOD".to_string(),
            net: None,
            remotes: Vec::new(),
            host: None,
            host_key: None,
            files: None,
            last_tick: -1,
            told: None,
            title: None,
            instance: {
                use std::hash::{BuildHasher, RandomState};
                RandomState::new().hash_one(std::time::SystemTime::now())
            },
            masters: Vec::new(),
            announced: None,
            started: Instant::now(),
        })
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Takes clients from the network at `addr` from now on, known to them by `identity`:
    /// where it listens.
    pub fn listen(&mut self, addr: SocketAddr, identity: Identity) -> std::io::Result<SocketAddr> {
        let net = gmod_net::Server::bind(addr, identity)?;
        let at = net.local_addr()?;
        self.net = Some(net);
        Ok(at)
    }

    /// The master servers it tells it is here, for the browsers' internet list: every minute,
    /// unless `sv_lan` is 1.
    pub fn set_masters(&mut self, masters: Vec<SocketAddr>) {
        self.masters = masters;
        self.announced = None;
    }

    /// Whether clients may join over the network.
    pub fn listening(&self) -> bool {
        self.net.is_some()
    }

    /// The player at this computer joins, known by `key` when they have one.
    pub fn join_host(&mut self, name: &str, key: Option<PublicKey>) -> Option<EntityHandle> {
        let steam_id64 = key.as_ref().map(keys::steam_id64);
        let player = self.runtime.join(PlayerInfo { name: name.to_string(), steam_id64, ..PlayerInfo::default() })?;
        self.host = Some(player);
        self.host_key = key;
        Some(player)
    }

    /// The players connected over the network: their names, their `SteamID`s, and their
    /// round trips.
    pub fn remote_players(&self) -> Vec<(String, String, Option<std::time::Duration>)> {
        let net = self.net.as_ref();
        self.remotes
            .iter()
            .map(|r| (r.name.clone(), keys::steam_id(keys::steam_id64(&r.key)), net.and_then(|n| n.client_info(r.id)).map(|(_, rtt)| rtt)))
            .collect()
    }

    /// One frame of `seconds`: what the clients sent, the game, and what goes to them.
    pub fn frame(&mut self, seconds: f64) -> ForHost {
        self.receive();
        self.runtime.think(seconds);
        self.tell_what_it_is();
        self.send()
    }

    /// What it is, for those who ask (the menu's server browser): once a second.
    fn tell_what_it_is(&mut self) {
        if self.net.is_none() || self.told.is_some_and(|at| at.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.told = Some(Instant::now());
        let title = self.title.get_or_insert_with(|| self.runtime.gamemode_title()).clone();
        let rows = self.runtime.scoreboard();
        let info = ServerInfo {
            name: self.name.clone(),
            map: self.map.clone(),
            gamemode: self.gamemode.clone(),
            title,
            players: rows.iter().filter(|r| !r.bot).count() as u32,
            max_players: self.runtime.max_players(),
            bots: rows.iter().filter(|r| r.bot).count() as u32,
            version: proto::VERSION,
            instance: self.instance,
            list: rows.into_iter().map(|r| (r.name, r.frags, r.time as f32)).collect(),
        };
        if let Some(net) = &mut self.net {
            net.set_info(&info.encode());
        }
        let lan = self.runtime.convar("sv_lan").is_some_and(|v| v.trim().parse::<f64>().is_ok_and(|n| n != 0.0));
        if !lan && !self.masters.is_empty() && self.announced.is_none_or(|at| at.elapsed() >= ANNOUNCE_EVERY) {
            self.announced = Some(Instant::now());
            if let Some(net) = &mut self.net {
                for master in &self.masters {
                    if let Err(e) = net.announce(*master) {
                        eprintln!("master server {master}: {e}");
                    }
                }
            }
        }
    }

    /// Tells every client the server goes, and lets them go.
    pub fn shut_down(&mut self, reason: &str) {
        if let Some(net) = &mut self.net {
            for remote in &self.remotes {
                net.disconnect(remote.id, reason);
            }
        }
        self.remotes.clear();
    }

    fn now(&self) -> Instant {
        Instant::now().max(self.started)
    }

    fn receive(&mut self) {
        let now = self.now();
        let Some(net) = &mut self.net else { return };
        for event in net.poll(now) {
            match event {
                ServerEvent::Request { addr, hello, key } => self.request(addr, &hello, key),
                ServerEvent::Message { client, data } => match proto::decode::<ToServer>(&data) {
                    Some(message) => self.message(client, message),
                    None => self.drop_client(client, "sent what is no message"),
                },
                ServerEvent::Disconnected { client, reason } => self.gone(client, &reason),
            }
        }
    }

    /// Someone asks to connect: let in if there is room and they speak our messages.
    fn request(&mut self, addr: SocketAddr, hello: &[u8], key: PublicKey) {
        let now = self.now();
        let Some(hello) = proto::decode::<Hello>(hello) else {
            self.deny(addr, "not an openGMOD client");
            return;
        };
        if hello.version != proto::VERSION {
            self.deny(addr, &format!("the server runs openGMOD's messages {}, the client {}", proto::VERSION, hello.version));
            return;
        }
        if self.host_key == Some(key) {
            self.deny(
                addr,
                "the player of this server's own computer has your key: another openGMOD folder (OPENGMOD_HOME) is another player",
            );
            return;
        }
        // the same player again (their game went, and came back before the server noticed):
        // the new connection is theirs
        if let Some(old) = self.remotes.iter().find(|r| r.key == key).map(|r| r.id) {
            if let Some(net) = &mut self.net {
                net.disconnect(old, "you connected again from elsewhere");
            }
            self.gone(old, "connected again");
        }
        let max = self.runtime.max_players() as usize;
        let taken = self.runtime.players().len() + self.remotes.iter().filter(|r| r.player.is_none()).count();
        if taken >= max {
            self.deny(addr, "the server is full");
            return;
        }
        let name: String = self.name.chars().take(64).collect();
        let allow_cs_lua = self.runtime.convar("sv_allowcslua").is_some_and(|v| v.trim().parse::<f64>().is_ok_and(|n| n != 0.0));
        // the welcome is one packet: a loading screen's address longer than any real one is left out
        let loading_url = self.runtime.convar("sv_loadingurl").filter(|u| u.len() <= 512).unwrap_or_default();
        let welcome = Welcome {
            server: name,
            map: self.map.clone(),
            gamemode: self.gamemode.clone(),
            max_players: max as u32,
            allow_cs_lua,
            loading_url,
        };
        let Some(net) = &mut self.net else { return };
        let Some(id) = net.accept(addr, &proto::encode(&welcome), now) else { return };
        let name = clean_name(&hello.name);
        println!("{name} is connecting from {addr}");
        self.remotes.push(Remote {
            id,
            addr,
            name,
            key,
            player: None,
            snapshots: SnapshotSender::default(),
            known: HashMap::new(),
            command: 0,
        });
        let (lua, content) = {
            let files = self.files();
            (files.lua.clone(), files.content.clone())
        };
        self.send_to(id, &FromServer::Files { lua, content }, true);
    }

    fn deny(&mut self, addr: SocketAddr, reason: &str) {
        if let Some(net) = &mut self.net {
            net.deny(addr, reason);
        }
    }

    /// The client Lua and the downloads, with what they are.
    fn files(&mut self) -> &Files {
        let named = (self.runtime.sent_files().len(), self.runtime.downloads().len());
        if self.files.as_ref().is_none_or(|f| f.named != named) {
            self.files = Some(self.list_files(named));
        }
        self.files.as_ref().expect("just made")
    }

    fn list_files(&self, named: (usize, usize)) -> Files {
        let info = |path: &str| self.vfs.read(path, "GAME").ok().map(|data| FileInfo::of(path, &data));
        // what the client Lua is: the files sent, and the gamemodes' descriptions it starts by
        let mut lua: Vec<FileInfo> = self.runtime.sent_files().iter().filter_map(|p| info(p)).collect();
        for gamemode in self.runtime.gamemodes() {
            lua.extend(info(&format!("gamemodes/{gamemode}/{gamemode}.txt")));
        }
        let mut content: Vec<FileInfo> = self.runtime.downloads().iter().filter(|p| downloadable(p)).filter_map(|p| info(p)).collect();
        content.extend(info(&format!("maps/{}.bsp", self.map)));
        Files { lua, content, named }
    }

    fn remote(&mut self, client: ClientId) -> Option<&mut Remote> {
        self.remotes.iter_mut().find(|r| r.id == client)
    }

    fn send_to(&mut self, client: ClientId, message: &FromServer, reliable: bool) {
        let Some(net) = &mut self.net else { return };
        let bytes = proto::encode(message);
        let reliable = reliable || bytes.len() > gmod_net::MAX_UNRELIABLE;
        if net.send(client, &bytes, reliable).is_err() {
            self.drop_client(client, "too much to send");
        }
    }

    fn message(&mut self, client: ClientId, message: ToServer) {
        let Some(remote) = self.remote(client) else { return };
        let player = remote.player;
        match message {
            ToServer::Want(paths) => self.send_files(client, &paths),
            ToServer::Ready if player.is_none() => {
                let (name, addr, steam_id64) = (remote.name.clone(), remote.addr, keys::steam_id64(&remote.key));
                let joined = self.runtime.join(PlayerInfo {
                    name: name.clone(),
                    address: addr.to_string(),
                    steam_id64: Some(steam_id64),
                    // its client predicts its movement and weapon
                    predicts: true,
                    ..PlayerInfo::default()
                });
                match joined {
                    Some(player) => {
                        if let Some(remote) = self.remote(client) {
                            remote.player = Some(player);
                        }
                        println!("{name} joined the game");
                        self.send_to(client, &FromServer::Joined { player }, true);
                    }
                    None => self.drop_client(client, "the server is full"),
                }
            }
            ToServer::Ready => {}
            ToServer::Commands { last, commands, ack, view } => {
                remote.snapshots.ack(ack);
                if let Some(player) = player
                    && let Some(cmd) = proto::merge_commands(remote.command, last, &commands)
                {
                    remote.command = last;
                    self.runtime.set_input(player, cmd);
                    if view > 0.0 {
                        self.runtime.set_view_time(player, view);
                    }
                }
            }
            ToServer::Command(line) => {
                if let Some(player) = player {
                    self.runtime.run_player_command(player, &line);
                }
            }
            ToServer::UserInfo(name, value) => {
                if let Some(player) = player {
                    self.runtime.set_user_info(player, &name, &value);
                }
            }
            ToServer::Net { name, data } => {
                if let Some(player) = player {
                    self.runtime.receive_net_message(&name, &data, player);
                }
            }
            ToServer::Button(code, down) => {
                if let Some(player) = player {
                    self.runtime.player_button_of(player, code, down);
                }
            }
        }
    }

    /// Sends a client the files it asked for, of those it was told of.
    fn send_files(&mut self, client: ClientId, paths: &[String]) {
        let files = self.files();
        let known: Vec<String> = paths.iter().filter(|p| files.lua.iter().chain(&files.content).any(|f| &f.path == *p)).cloned().collect();
        for path in known {
            let Ok(data) = self.vfs.read(&path, "GAME") else { continue };
            let size = data.len() as u64;
            let mut offset = 0;
            loop {
                let end = (offset + FILE_PIECE).min(data.len());
                let piece = FromServer::File { path: path.clone(), offset: offset as u64, size, data: data[offset..end].to_vec() };
                self.send_to(client, &piece, true);
                offset = end;
                if offset >= data.len() {
                    break;
                }
            }
        }
        self.send_to(client, &FromServer::FilesSent, true);
    }

    /// Lets a client go that broke the rules or could not be served.
    fn drop_client(&mut self, client: ClientId, reason: &str) {
        if let Some(net) = &mut self.net {
            net.disconnect(client, reason);
        }
        self.gone(client, reason);
    }

    /// A client is gone: its player leaves.
    fn gone(&mut self, client: ClientId, reason: &str) {
        let Some(i) = self.remotes.iter().position(|r| r.id == client) else { return };
        let remote = self.remotes.remove(i);
        println!("{} left the game ({reason})", remote.name);
        if let Some(player) = remote.player {
            self.runtime.leave(player);
        }
    }

    /// What the frame made for each: snapshots, the chat, `net`, sounds, what Lua asked.
    fn send(&mut self) -> ForHost {
        let mut host = ForHost::default();
        let joined: Vec<(ClientId, EntityHandle)> = self.remotes.iter().filter_map(|r| Some((r.id, r.player?))).collect();
        let route = |player: EntityHandle| -> Option<Option<ClientId>> {
            if Some(player) == self.host {
                return Some(None);
            }
            joined.iter().find(|(_, p)| *p == player).map(|(id, _)| Some(*id))
        };
        let mut out: Vec<(ClientId, FromServer, bool)> = Vec::new();
        for (to, message) in self.runtime.take_chat() {
            match route(to) {
                Some(None) => host.chat.push(message),
                Some(Some(id)) => out.push((id, FromServer::Chat(message), true)),
                None => {}
            }
        }
        for message in self.runtime.take_net_messages() {
            for &index in &message.to {
                let Some(player) = self.runtime.players().into_iter().find(|p| p.index == Some(index)) else { continue };
                match route(player) {
                    Some(None) => host.net.push(message.clone()),
                    Some(Some(id)) => {
                        out.push((id, FromServer::Net { name: message.name.clone(), data: message.data.clone() }, !message.unreliable))
                    }
                    None => {}
                }
            }
        }
        for (to, asked) in self.runtime.take_to_clients() {
            match route(to) {
                Some(None) => host.asked.push(asked),
                Some(Some(id)) => out.push((id, FromServer::Asked(asked), true)),
                None => {}
            }
        }
        // each hears what is heard where its player is
        let sounds = self.runtime.take_sounds();
        if !sounds.is_empty() {
            let hearers: Vec<Option<Vec<EntityHandle>>> = sounds.iter().map(|s| self.runtime.hearing(s)).collect();
            let heard = |player: EntityHandle| -> Vec<gmod_lua::sound::SoundEvent> {
                sounds
                    .iter()
                    .zip(&hearers)
                    .filter(|(s, h)| h.as_ref().is_none_or(|h| h.contains(&player)) && s.unheard_by() != Some(player))
                    .map(|(s, _)| s.clone())
                    .collect()
            };
            for (id, player) in &joined {
                let mine = heard(*player);
                if !mine.is_empty() {
                    out.push((*id, FromServer::Sounds(mine), true));
                }
            }
            host.sounds = match self.host {
                Some(player) => heard(player),
                None => Vec::new(),
            };
        }
        // the entities, once a tick has passed
        let tick = (self.runtime.cur_time() / gmod_lua::timer::TICK_INTERVAL).floor() as i64;
        if tick != self.last_tick && !joined.is_empty() {
            self.last_tick = tick;
            let snapshot = self.runtime.snapshot();
            let serials: HashMap<u32, u32> = snapshot.entities.iter().map(|e| (e.index, e.serial)).collect();
            let encoded = proto::encode_entities(&snapshot);
            let transmission = self.runtime.transmission();
            let time = self.runtime.cur_time();
            for remote in self.remotes.iter_mut() {
                let Some(player) = remote.player else { continue };
                // each is sent what its player may see; what it has of the rest it keeps dormant
                let sent = self.runtime.sent_to(&transmission, player);
                remote.known.retain(|index, serial| serials.get(index) == Some(serial));
                let dormant: Vec<(u32, u32)> =
                    sent.held.iter().filter(|(index, serial)| remote.known.get(index) == Some(serial)).copied().collect();
                for index in &sent.indices {
                    if let Some(&serial) = serials.get(index) {
                        remote.known.insert(*index, serial);
                    }
                }
                let mine = Arc::new(proto::for_player(&encoded, &sent.indices, &dormant));
                let delta = remote.snapshots.delta(&mine, time, remote.command);
                out.push((remote.id, FromServer::Snapshot(delta), false));
            }
        }
        for (id, message, reliable) in out {
            self.send_to(id, &message, reliable);
        }
        let now = self.now();
        if let Some(net) = &mut self.net {
            net.flush(now);
        }
        host
    }
}

/// A player's name as the server takes it: printable, not too long, not empty.
fn clean_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(32).collect();
    let name = name.trim();
    if name.is_empty() { "unnamed".to_string() } else { name.to_string() }
}

/// What a server may have its clients download: the game's content, never code.
pub fn downloadable(path: &str) -> bool {
    const KINDS: &[&str] = &[
        "bsp",
        "nav",
        "ain",
        "mdl",
        "vvd",
        "vtx",
        "phy",
        "ani",
        "jpg",
        "png",
        "vmt",
        "vtf",
        "wav",
        "mp3",
        "ogg",
        "pcf",
        "ttf",
        "txt",
        "res",
        "properties",
    ];
    let Some(path) = gmod_fs::path::normalize(path) else { return false };
    let extension = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    KINDS.contains(&extension) && !path.starts_with("lua/") && !path.starts_with("cfg/") && !path.starts_with("bin/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_downloads_are_kept_in_bounds() {
        assert_eq!(clean_name("  Al\u{7}ice  "), "Alice");
        assert_eq!(clean_name("\n"), "unnamed");
        assert_eq!(clean_name(&"x".repeat(100)).len(), 32);
        assert!(downloadable("models/thing.mdl") && downloadable("maps/gm_x.bsp") && downloadable("sound/a/b.wav"));
        assert!(!downloadable("lua/autorun/x.lua") && !downloadable("bin/client.dll") && !downloadable("../evil.mdl"));
        assert!(!downloadable("cfg/autoexec.txt") && !downloadable("models/x.exe"));
    }
}
