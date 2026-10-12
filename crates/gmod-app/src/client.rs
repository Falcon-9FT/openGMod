//! A client of a server over the network ([`proto`](crate::proto)): connecting, getting the
//! files the server says to have, and then the game as the server sends it - snapshots of
//! the entities, drawn a little behind the newest and between two of them, so that what
//! moves moves smoothly whatever the network does - and what the player does, sent back.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gmod_fs::Vfs;
use gmod_lua::net::Bits;
use gmod_lua::sound::SoundEvent;
use gmod_lua::{ChatMessage, EntityHandle, Snapshot, ToClient, UserCmd};
use gmod_net::ClientEvent;

use crate::proto::{self, FileInfo, FromServer, Hello, SnapshotReceiver, ToServer, Welcome};

/// How far behind the newest snapshot the picture is: a snapshot or two lost on the way
/// still leaves two to draw between.
const INTERPOLATION: f64 = 0.1;
/// How many of the latest commands each packet of commands carries.
const COMMANDS_SENT: usize = 4;
/// How many commands are kept to move the player on by while the server has not answered.
const MAX_UNANSWERED: usize = 256;
/// How long to wait for the server to answer, and to hear from it again while it sends the
/// files and lets the player join.
const CONNECT_FOR: Duration = Duration::from_secs(12);
const QUIET_FOR: Duration = Duration::from_secs(30);
/// The largest file a server may send.
const MAX_FILE: u64 = 512 << 20;

/// What came from the server besides the entities.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Joined(EntityHandle),
    Chat(ChatMessage),
    Net { name: String, data: Bits },
    Sounds(Vec<SoundEvent>),
    Asked(ToClient),
}

/// A connection to a server.
pub struct Connection {
    net: gmod_net::Client,
    pub welcome: Welcome,
    snapshots: SnapshotReceiver,
    /// The commands sent and not yet answered by a snapshot, numbered, with how long each
    /// was the player's.
    commands: VecDeque<(u32, UserCmd, f32)>,
    last_command: u32,
    /// The last command the newest snapshot answers.
    answered: u32,
    /// Snapshots received, oldest first, to draw between.
    buffer: VecDeque<Snapshot>,
    /// The server's time the picture shows.
    shown: Option<f64>,
    /// What came besides snapshots and has not been taken, oldest first.
    inbox: VecDeque<FromServer>,
    /// Why the connection closed, once it has.
    pub closed: Option<String>,
    /// The client Lua the server has, by game path ([`download`](Self::download)): what of
    /// the player's addons it may run.
    pub listed_lua: Vec<String>,
}

/// What fetching a server's files comes to, as it goes: the files it is to fetch, then each as
/// it starts coming.
pub enum Fetching<'a> {
    Listed(&'a [String]),
    Coming(&'a str),
}

/// Waits a moment between looks at the network while blocking on it.
fn nap() {
    std::thread::sleep(Duration::from_millis(5));
}

/// What of the player's addons a server's client does not run, as in GMod: their Lua that the
/// server does not have (`listed`). Their content is used all the same.
pub fn addon_lua_hidden(listed: &[String]) -> gmod_fs::Hide {
    let listed: HashSet<String> = listed.iter().filter_map(|p| gmod_fs::path::normalize(p)).collect();
    Arc::new(move |path: &str| {
        let path = path.to_ascii_lowercase();
        path.ends_with(".lua") && (path.starts_with("lua/") || path.starts_with("gamemodes/")) && !listed.contains(&path)
    })
}

impl Connection {
    /// Connects to a server as `name`, waiting until it lets us in or says why not.
    pub fn open(server: SocketAddr, name: &str) -> Result<Connection, String> {
        let hello = proto::encode(&Hello { version: proto::VERSION, name: name.to_string() });
        let started = Instant::now();
        let net = gmod_net::Client::connect(server, &hello, started).map_err(|e| format!("{server}: {e}"))?;
        let mut connection = Connection {
            net,
            welcome: Welcome {
                server: String::new(),
                map: String::new(),
                gamemode: String::new(),
                max_players: 1,
                allow_cs_lua: false,
                loading_url: String::new(),
            },
            listed_lua: Vec::new(),
            snapshots: SnapshotReceiver::default(),
            commands: VecDeque::new(),
            last_command: 0,
            answered: 0,
            buffer: VecDeque::new(),
            shown: None,
            inbox: VecDeque::new(),
            closed: None,
        };
        loop {
            if let Some(welcome) = connection.receive() {
                connection.welcome = proto::decode::<Welcome>(&welcome).ok_or("the server is not an openGMOD server")?;
                return Ok(connection);
            }
            if let Some(reason) = &connection.closed {
                return Err(format!("{server}: {reason}"));
            }
            if started.elapsed() > CONNECT_FOR {
                return Err(format!("{server}: the server does not answer"));
            }
            nap();
        }
    }

    /// Reads what arrived - snapshots into the buffer, the rest into the inbox - and sends
    /// what is due. The welcome, when that came.
    fn receive(&mut self) -> Option<Vec<u8>> {
        let now = Instant::now();
        let mut welcomed = None;
        for event in self.net.poll(now) {
            match event {
                ClientEvent::Accepted { welcome } => welcomed = Some(welcome),
                ClientEvent::Message { data } => match proto::decode::<FromServer>(&data) {
                    Some(FromServer::Snapshot(delta)) => self.snapshot(delta),
                    Some(message) => self.inbox.push_back(message),
                    None => {
                        self.net.disconnect("the server sent what is no message");
                        self.closed = Some("the server sent what is no message".into());
                    }
                },
                ClientEvent::Disconnected { reason } | ClientEvent::Denied { reason } => self.closed = Some(reason),
            }
        }
        self.net.flush(now);
        welcomed
    }

    /// Waits, reading, until `take` takes something out of the inbox, the connection closes,
    /// or the server has been quiet for too long.
    fn wait_for<T>(&mut self, what: &str, mut take: impl FnMut(&mut Connection) -> Option<T>) -> Result<T, String> {
        let mut heard = Instant::now();
        loop {
            let before = self.inbox.len() + self.buffer.len();
            self.receive();
            if self.inbox.len() + self.buffer.len() != before {
                heard = Instant::now();
            }
            if let Some(taken) = take(self) {
                return Ok(taken);
            }
            if let Some(reason) = &self.closed {
                return Err(reason.clone());
            }
            if heard.elapsed() > QUIET_FOR {
                return Err(format!("the server stopped before {what}"));
            }
            nap();
        }
    }

    /// The first message in the inbox `pick` wants, taken out.
    fn take<T>(&mut self, mut pick: impl FnMut(&FromServer) -> bool, into: impl FnOnce(FromServer) -> Option<T>) -> Option<T> {
        let i = self.inbox.iter().position(&mut pick)?;
        into(self.inbox.remove(i)?)
    }

    pub fn send(&mut self, message: &ToServer, reliable: bool) {
        let _ = self.net.send(&proto::encode(message), reliable);
    }

    /// Gets the files the server says to have that `vfs` does not have as the server has them:
    /// the client Lua, which is the server's (mounted in memory for the game), and the content
    /// the game does not have, saved into `download` (mounted behind the game's own files, so
    /// that what is there is downloaded again only when the server has it otherwise). Waits
    /// until all came.
    pub fn download(&mut self, vfs: &Vfs, download: Option<&Path>, told: &dyn Fn(Fetching)) -> Result<Vec<(String, Vec<u8>)>, String> {
        let (listed_lua, listed_content) = self.wait_for("saying what files to have", |c| {
            c.take(
                |m| matches!(m, FromServer::Files { .. }),
                |m| match m {
                    FromServer::Files { lua, content } => Some((lua, content)),
                    _ => None,
                },
            )
        })?;
        self.listed_lua = listed_lua.iter().map(|f| f.path.clone()).collect();
        let same = |f: &FileInfo| vfs.read(&f.path, "GAME").is_ok_and(|data| FileInfo::of(&f.path, &data) == *f);
        let downloaded = |f: &FileInfo| download.is_some_and(|dir| vfs.locate(&f.path, "GAME").is_some_and(|m| m.origin() == dir));
        let lua: Vec<FileInfo> = listed_lua.into_iter().filter(|f| !same(f)).collect();
        let content: Vec<FileInfo> = listed_content
            .into_iter()
            .filter(|f| crate::server::downloadable(&f.path))
            .filter(|f| vfs.locate(&f.path, "GAME").is_none() || (downloaded(f) && !same(f)))
            .collect();
        let wanted: Vec<&FileInfo> = lua.iter().chain(&content).collect();
        told(Fetching::Listed(&wanted.iter().map(|f| f.path.clone()).collect::<Vec<_>>()));
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let bytes: u64 = wanted.iter().map(|f| f.size).sum();
        println!("Downloading {} files ({:.1} MB) from the server", wanted.len(), bytes as f64 / 1e6);
        self.send(&ToServer::Want(wanted.iter().map(|f| f.path.clone()).collect()), true);
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        let mut wrong = None;
        self.wait_for("sending the files", |c| {
            while let Some(piece) = c.take(|m| matches!(m, FromServer::File { .. } | FromServer::FilesSent), Some) {
                let FromServer::File { path, offset, size, data } = piece else { return Some(()) };
                let Some(info) = wanted.iter().find(|f| f.path == path) else { continue };
                if offset == 0 {
                    told(Fetching::Coming(&path));
                }
                let file = files.entry(path).or_default();
                if offset != file.len() as u64 || size != info.size || size > MAX_FILE || offset + data.len() as u64 > size {
                    wrong = Some(format!("{}: sent wrong", info.path));
                    return Some(());
                }
                file.extend_from_slice(&data);
            }
            None
        })?;
        if let Some(wrong) = wrong {
            return Err(wrong);
        }
        let mut lua_files = Vec::new();
        for info in wanted {
            let data = files.remove(&info.path).unwrap_or_default();
            if FileInfo::of(&info.path, &data) != *info {
                return Err(format!("{}: did not come whole", info.path));
            }
            if lua.contains(info) {
                lua_files.push((info.path.clone(), data));
            } else if let Some(dir) = download
                && let Some(path) = gmod_fs::path::normalize(&info.path)
            {
                let file = dir.join(&path);
                let saved = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&file, &data));
                if let Err(e) = saved {
                    return Err(format!("{}: {e}", file.display()));
                }
            }
        }
        // the download folder, read while looking for what to fetch, has more in it now
        if let Some(dir) = download {
            for mount in vfs.mounts() {
                if let gmod_fs::Backing::Dir(folder) = &mount.backing
                    && folder.root() == dir
                {
                    folder.forget();
                }
            }
        }
        Ok(lua_files)
    }

    /// The client has loaded: its player joins. Waits until it has.
    pub fn join(&mut self) -> Result<EntityHandle, String> {
        self.send(&ToServer::Ready, true);
        self.wait_for("letting the player join", |c| {
            c.take(
                |m| matches!(m, FromServer::Joined { .. }),
                |m| match m {
                    FromServer::Joined { player } => Some(player),
                    _ => None,
                },
            )
        })
    }

    fn snapshot(&mut self, delta: proto::SnapshotDelta) {
        let command = delta.command;
        if let Some(snapshot) = self.snapshots.receive(delta) {
            // one older than the newest came late: it is no use
            if self.buffer.back().is_none_or(|b| b.time < snapshot.time) {
                self.buffer.push_back(snapshot);
                self.answered = self.answered.max(command);
                while self.commands.front().is_some_and(|(n, _, _)| *n <= self.answered) {
                    self.commands.pop_front();
                }
            }
        }
    }

    /// The player's own entity in the newest snapshot: where the server last said it was.
    pub fn newest(&self, player: EntityHandle) -> Option<&gmod_lua::EntityState> {
        let index = player.index?;
        self.buffer.back()?.entities.iter().find(|e| e.index == index && e.serial == player.serial)
    }

    /// The commands the newest snapshot does not answer yet, oldest first, with how long each
    /// was the player's: what to move the player on by from there.
    pub fn unanswered(&self) -> Vec<(UserCmd, f32)> {
        self.commands.iter().map(|&(_, cmd, seconds)| (cmd, seconds)).collect()
    }

    /// Reads what arrived and sends what is due: what happened since the last call.
    pub fn poll(&mut self) -> Vec<Event> {
        self.receive();
        self.inbox.drain(..).filter_map(event_of).collect()
    }

    /// The player's command of this frame of `seconds`, sent with those before it (one lost on
    /// the way comes with the next) and the newest snapshot here.
    pub fn command(&mut self, cmd: UserCmd, seconds: f32) {
        self.last_command += 1;
        self.commands.push_back((self.last_command, cmd, seconds));
        // a server that answers nothing: only the latest are kept
        while self.commands.len() > MAX_UNANSWERED {
            self.commands.pop_front();
        }
        let first = self.commands.len().saturating_sub(COMMANDS_SENT);
        let sent = self.commands.iter().skip(first).map(|&(_, cmd, _)| cmd).collect();
        let view = self.shown.unwrap_or(0.0);
        let message = ToServer::Commands { last: self.last_command, commands: sent, ack: self.snapshots.newest, view };
        self.send(&message, false);
        self.net.flush(Instant::now());
    }

    /// What to show after `seconds` more: the entities a little behind the newest snapshot,
    /// between the two around that moment.
    pub fn shown(&mut self, seconds: f64) -> Option<Snapshot> {
        let target = self.buffer.back()?.time - INTERPOLATION;
        let time = match self.shown {
            // far off (a hitch, the start): to where it should be at once; near, drawn there
            // gently, which keeps the motion even
            Some(shown) if (shown + seconds - target).abs() < 0.25 => {
                let next = shown + seconds;
                next + (target - next) * 0.05
            }
            _ => target,
        };
        self.shown = Some(time);
        while self.buffer.len() > 2 && self.buffer[1].time <= time {
            self.buffer.pop_front();
        }
        let a = self.buffer.front()?;
        Some(match self.buffer.get(1) {
            Some(b) if b.time > a.time && time > a.time => Snapshot::between(a, b, ((time - a.time) / (b.time - a.time)) as f32),
            _ => a.clone(),
        })
    }

    /// Leaves the server, saying so.
    pub fn close(&mut self, reason: &str) {
        self.net.disconnect(reason);
    }
}

fn event_of(message: FromServer) -> Option<Event> {
    Some(match message {
        FromServer::Joined { player } => Event::Joined(player),
        FromServer::Chat(message) => Event::Chat(message),
        FromServer::Net { name, data } => Event::Net { name, data },
        FromServer::Sounds(sounds) => Event::Sounds(sounds),
        FromServer::Asked(asked) => Event::Asked(asked),
        FromServer::Files { .. } | FromServer::File { .. } | FromServer::FilesSent | FromServer::Snapshot(_) => return None,
    })
}
