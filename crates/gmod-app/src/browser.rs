//! The menu's server browser, its network side: what `serverlist` asks
//! ([`gmod_lua::ServerQuery`]) is asked of servers with [`gmod_net::Query`], and their answers
//! ([`ServerInfo`]) go back to the menu.
//!
//! The local network is searched by a question to the broadcast address on the ports servers
//! take (27015 to 27020), and to this computer's own; favourites and the servers played on are
//! asked one by one, and kept in openGMOD's folder (`servers/favorites.txt`,
//! `servers/history.txt`). The internet's list is what the master servers the player names
//! (`servers/masters.txt`; none by default) list, page by page ([`gmod_net::Master`]), each
//! server then asked itself; servers tell the same masters they are here.

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gmod_lua::{Runtime, ServerEntry, ServerPlayer, ServerQuery};
use gmod_net::{Answer, Page, Reply};

use crate::proto::{self, ServerInfo};

/// How long a list waits for answers, and a single server for its.
const LIST_FOR: Duration = Duration::from_millis(1500);
/// How long the internet's list waits: for the masters' pages, then the servers on them.
const INTERNET_FOR: Duration = Duration::from_secs(5);
const PING_FOR: Duration = Duration::from_secs(2);
/// The ports a local network's servers are looked for at.
const LAN_PORTS: std::ops::RangeInclusive<u16> = 27015..=27020;
/// How many servers played on are remembered.
const HISTORY: usize = 100;

/// A list being made: its questions and what came of them.
struct Listing {
    id: u64,
    until: Instant,
    /// The servers asked one by one, by question, with the address the menu knows them by.
    asked: HashMap<u64, String>,
    /// Questions to many (broadcasts): any answer is a server.
    broadcasts: HashSet<u64>,
    /// Pages asked of master servers, by question, with the master.
    masters: HashMap<u64, SocketAddr>,
    /// The servers the masters' pages named, asked already.
    named: HashSet<SocketAddr>,
    answered: HashSet<SocketAddr>,
    /// The servers listed, by what they picked as they started: one at several addresses is
    /// listed once.
    servers: HashSet<u64>,
    /// The menu said it has enough.
    stopped: bool,
}

/// A single server asked: by `PingServer` or `PlayerList`.
struct Single {
    id: u64,
    question: Option<u64>,
    until: Instant,
    players: bool,
}

/// The browser's state between frames.
#[derive(Default)]
pub struct Browser {
    query: Option<gmod_net::Query>,
    listings: Vec<Listing>,
    singles: Vec<Single>,
    /// Where favourites and the servers played on are kept.
    folder: Option<PathBuf>,
}

/// An address the menu names: `host` (the default port) or `host:port`.
fn resolve(address: &str) -> Option<SocketAddr> {
    resolve_at(address, proto::DEFAULT_PORT)
}

/// An address, `host` (at `port`) or `host:port`.
fn resolve_at(address: &str, port: u16) -> Option<SocketAddr> {
    if let Ok(ip) = address.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(ip, port));
    }
    let with_port = match address.rsplit_once(':') {
        Some((_, p)) if p.parse::<u16>().is_ok() => address.to_string(),
        _ => format!("{address}:{port}"),
    };
    with_port.to_socket_addrs().ok()?.next()
}

/// The master servers: those `servers/masters.txt` of openGMOD's folder names, one a line,
/// and `more`; at their default port unless they say.
pub fn masters(more: &[String]) -> Vec<SocketAddr> {
    masters_in(gmod_fs::UserDir::default_root().map(|root| root.join("servers")).as_deref(), more)
}

/// The master servers `masters.txt` in `folder` names, and `more`.
fn masters_in(folder: Option<&std::path::Path>, more: &[String]) -> Vec<SocketAddr> {
    let mut names = folder.map(|f| read_list(&f.join("masters.txt"))).unwrap_or_default();
    names.extend(more.iter().cloned());
    let mut out = Vec::new();
    for name in names.iter().filter(|n| !n.starts_with('#')) {
        match resolve_at(name, gmod_net::MASTER_PORT) {
            Some(addr) if !out.contains(&addr) => out.push(addr),
            Some(_) => {}
            None => eprintln!("master server {name}: no such address"),
        }
    }
    out
}

/// A line of the browser for a server's answer.
fn entry(from: SocketAddr, took: Duration, info: &ServerInfo) -> ServerEntry {
    ServerEntry {
        address: from.to_string(),
        ping: took.as_millis().min(9999) as u32,
        name: info.name.clone(),
        description: info.title.clone(),
        map: info.map.clone(),
        players: info.players,
        max_players: info.max_players,
        bots: info.bots,
        password: false,
        gamemode: info.gamemode.clone(),
        workshop_id: String::new(),
        version: info.version.to_string(),
    }
}

/// The addresses in a file of them, one a line.
fn read_list(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path).unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

impl Browser {
    /// A browser that keeps its lists in `folder` (none: it keeps nothing).
    pub fn new(folder: Option<PathBuf>) -> Browser {
        Browser { folder, ..Browser::default() }
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        self.folder.as_ref().map(|f| f.join(name))
    }

    fn write_list(&self, name: &str, list: &[String]) {
        let Some(path) = self.file(name) else { return };
        let saved = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, list.join("\n") + "\n"));
        if let Err(e) = saved {
            eprintln!("{}: {e}", path.display());
        }
    }

    /// The favourite servers, for the menu.
    pub fn favorites(&self) -> Vec<String> {
        self.file("favorites.txt").map(|p| read_list(&p)).unwrap_or_default()
    }

    /// A server was played on: it heads the history.
    pub fn played_on(&self, address: &str) {
        let Some(path) = self.file("history.txt") else { return };
        let mut list = read_list(&path);
        list.retain(|a| !a.eq_ignore_ascii_case(address));
        list.insert(0, address.to_string());
        list.truncate(HISTORY);
        self.write_list("history.txt", &list);
    }

    fn query(&mut self) -> Option<&mut gmod_net::Query> {
        if self.query.is_none() {
            match gmod_net::Query::open() {
                Ok(q) => self.query = Some(q),
                Err(e) => {
                    eprintln!("The server browser cannot use the network: {e}");
                    return None;
                }
            }
        }
        self.query.as_mut()
    }

    /// Takes what the menu asked, asks the network, hands back what came and what did not.
    pub fn poll(&mut self, menu: &Runtime) {
        let now = Instant::now();
        if let Some(favorites) = menu.take_favorite_servers() {
            self.write_list("favorites.txt", &favorites);
        }
        for asked in menu.take_server_queries() {
            self.start(menu, asked, now);
        }
        let replies = self.query.as_mut().map(|q| q.poll(now)).unwrap_or_default();
        for reply in replies {
            match reply {
                Reply::Info(answer) => self.answered(menu, answer),
                Reply::Page(page) => self.paged(page, now),
            }
        }
        // what waited long enough
        for single in self.singles.iter().filter(|s| now >= s.until) {
            if single.players {
                menu.server_players(single.id, &[]);
            } else {
                menu.server_pinged(single.id, None);
            }
        }
        self.singles.retain(|s| now < s.until);
        let (done, waiting): (Vec<Listing>, Vec<Listing>) = std::mem::take(&mut self.listings)
            .into_iter()
            .partition(|l| now >= l.until || l.stopped || (l.asked.is_empty() && l.broadcasts.is_empty() && l.masters.is_empty()));
        self.listings = waiting;
        for listing in done {
            if !listing.stopped {
                for address in listing.asked.values() {
                    if !menu.server_unanswered(listing.id, address) {
                        break;
                    }
                }
            }
            menu.servers_listed(listing.id);
        }
    }

    /// A server's answer: to a single question, or for a list.
    fn answered(&mut self, menu: &Runtime, answer: Answer) {
        let Some(info) = ServerInfo::decode(&answer.info) else { return };
        if let Some(single) = self.singles.iter().position(|s| s.question == Some(answer.question)) {
            let single = self.singles.remove(single);
            if single.players {
                let players: Vec<ServerPlayer> = info
                    .list
                    .iter()
                    .map(|(name, frags, time)| ServerPlayer { name: name.clone(), score: *frags as i32, time: *time })
                    .collect();
                menu.server_players(single.id, &players);
            } else {
                menu.server_pinged(single.id, Some(&entry(answer.from, answer.took, &info)));
            }
            return;
        }
        let Some(listing) =
            self.listings.iter_mut().find(|l| l.asked.contains_key(&answer.question) || l.broadcasts.contains(&answer.question))
        else {
            return;
        };
        // a server is listed once, though it answers on several ways
        let new_address = listing.answered.insert(answer.from);
        listing.asked.remove(&answer.question);
        if listing.stopped || !new_address || !listing.servers.insert(info.instance) {
            return;
        }
        listing.stopped = !menu.server_listed(listing.id, &entry(answer.from, answer.took, &info));
    }

    /// A page of a master's list: each server on it is asked, and the next page.
    fn paged(&mut self, page: Page, now: Instant) {
        let Some(listing) = self.listings.iter_mut().find(|l| l.masters.contains_key(&page.question)) else { return };
        let Some(master) = listing.masters.remove(&page.question) else { return };
        let Some(query) = self.query.as_mut() else { return };
        if listing.stopped {
            return;
        }
        for &server in &page.servers {
            if listing.named.insert(server)
                && let Ok(question) = query.ask(server, now)
            {
                listing.asked.insert(question, server.to_string());
            }
        }
        let next = page.start.saturating_add(page.servers.len() as u32);
        if !page.servers.is_empty()
            && next < page.total
            && let Ok(question) = query.ask_master(master, next, now)
        {
            listing.masters.insert(question, master);
        }
    }

    fn start(&mut self, menu: &Runtime, asked: ServerQuery, now: Instant) {
        let players = matches!(asked, ServerQuery::Players { .. });
        match asked {
            ServerQuery::List { id, list } => {
                let mut listing = Listing {
                    id,
                    until: now + if list == "internet" { INTERNET_FOR } else { LIST_FOR },
                    asked: HashMap::new(),
                    broadcasts: HashSet::new(),
                    masters: HashMap::new(),
                    named: HashSet::new(),
                    answered: HashSet::new(),
                    servers: HashSet::new(),
                    stopped: false,
                };
                let addresses = match list.as_str() {
                    "favorite" => self.favorites(),
                    "history" => self.file("history.txt").map(|p| read_list(&p)).unwrap_or_default(),
                    _ => Vec::new(),
                };
                let masters = if list == "internet" { masters_in(self.folder.as_deref(), &[]) } else { Vec::new() };
                if let Some(query) = self.query() {
                    for master in masters {
                        if let Ok(question) = query.ask_master(master, 0, now) {
                            listing.masters.insert(question, master);
                        }
                    }
                    if list == "lan" {
                        for port in LAN_PORTS {
                            for to in [[255, 255, 255, 255], [127, 0, 0, 1]] {
                                if let Ok(q) = query.ask(SocketAddr::from((to, port)), now) {
                                    listing.broadcasts.insert(q);
                                }
                            }
                        }
                    }
                    for address in addresses {
                        match resolve(&address).map(|to| query.ask(to, now)) {
                            Some(Ok(q)) => {
                                listing.asked.insert(q, address);
                            }
                            // one that names no server is a failure at once
                            _ => {
                                listing.asked.insert(u64::MAX - listing.asked.len() as u64, address);
                            }
                        }
                    }
                } else {
                    for (i, address) in addresses.into_iter().enumerate() {
                        listing.asked.insert(u64::MAX - i as u64, address);
                    }
                }
                // no master server named: the internet's list has nothing
                if listing.asked.is_empty() && listing.broadcasts.is_empty() && listing.masters.is_empty() {
                    menu.servers_listed(id);
                } else {
                    self.listings.push(listing);
                }
            }
            ServerQuery::Ping { id, address } | ServerQuery::Players { id, address } => {
                let question = resolve(&address).and_then(|to| self.query()?.ask(to, now).ok());
                // a name that is no server times out at once
                let until = if question.is_some() { now + PING_FOR } else { now };
                self.singles.push(Single { id, question, until, players });
            }
        }
    }
}
