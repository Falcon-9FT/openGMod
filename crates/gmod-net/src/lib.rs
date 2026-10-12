//! openGMOD's network: its own protocol over UDP. Source's protocol is not documented, and an
//! unofficial client on official servers would put players at odds with their anti-cheat, so
//! openGMOD servers talk to openGMOD clients only (see `docs/ARCHITECTURE.md`).
//!
//! What this crate gives is a connection that carries messages - byte strings - two ways:
//!
//! * reliable ones arrive whole, once and in the order they were sent, however long they are
//!   (a snapshot of a whole map, the client Lua of a server): they go in chunks that are sent
//!   again until the other side says it has them;
//! * unreliable ones arrive whole or not at all, maybe out of order: what is stale by the time
//!   it would be sent again (where entities are, the keys a player holds). One too big for a
//!   packet goes in fragments, and is lost if any is.
//!
//! A packet starts with `OGMN` and its kind:
//!
//! * `CONNECT` (client to server): the protocol's version, a number the client picked for this
//!   attempt, and what the client has to say (its name...). Sent again every quarter second
//!   until it is answered, for ten seconds.
//! * `ACCEPT`: the client's number, the connection's (picked by the server), and what the
//!   server answers (the map...). `DENY`: the client's number and why not.
//! * `DATA`: the connection, the packet's sequence number and the newest one received from the
//!   other side (for the round trip), the reliable chunks received - all before a number, and
//!   up to eight ranges of those that came early - then the messages. Every packet
//!   acknowledges what arrived, so a packet lost costs nothing more than what was in it: its
//!   reliable chunks are sent again when they have waited longer than the round trip. A packet
//!   that comes twice is dropped the second time.
//! * `DISCONNECT`: the connection, and why.
//!
//! A side that hears nothing for 15 seconds drops the connection; one with nothing to say still
//! sends an empty `DATA` packet ten times a second, which keeps the other side's
//! acknowledgements flowing. Nothing read from the network can make it panic or grow without
//! bound: lengths are checked, a reliable message may hold 64 MiB, and chunks too far ahead
//! are dropped.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// The version of the protocol: a client and a server of different ones do not connect.
pub const PROTOCOL: u16 = 1;

const MAGIC: [u8; 4] = *b"OGMN";
const CONNECT: u8 = 1;
const ACCEPT: u8 = 2;
const DENY: u8 = 3;
const DATA: u8 = 4;
const DISCONNECT: u8 = 5;

const RELIABLE: u8 = 1;
const UNRELIABLE: u8 = 2;
const FRAGMENT: u8 = 3;

/// The most a packet holds: under what any path between two computers carries whole.
pub const MAX_PACKET: usize = 1200;
/// The header of a `DATA` packet at its longest: magic, kind, connection, sequence, the newest
/// packet received, the chunks received and their ranges.
const DATA_HEADER: usize = 4 + 1 + 8 + 2 + 2 + 4 + 1 + RANGES * 6;
/// How many ranges of chunks that came early a packet acknowledges.
const RANGES: usize = 8;
/// How many packets back one that comes twice is still known.
const SEEN: usize = 1024;
/// What a reliable chunk carries at most.
const CHUNK: usize = 1000;
/// What a fragment of an unreliable message carries at most.
const FRAGMENT_BYTES: usize = MAX_PACKET - DATA_HEADER - 1 - 2 - 1 - 1 - 2;
/// The largest reliable message.
pub const MAX_RELIABLE: usize = 64 << 20;
/// The most a client's hello and a server's welcome may hold: each goes in one packet.
pub const MAX_HELLO: usize = 1000;
/// The largest unreliable message: 255 fragments.
pub const MAX_UNRELIABLE: usize = FRAGMENT_BYTES * 255;
/// How many reliable chunks may be out at once, waiting to be acknowledged.
const IN_FLIGHT: usize = 1024;
/// How far ahead of the next expected chunk one is still kept.
const WINDOW: u32 = 4096;
/// How many packets one flush sends at most; what does not fit goes with the next.
const PACKETS_PER_FLUSH: usize = 512;
/// How fast reliable chunks go out by default, in bytes a second: a big message (the client
/// Lua of a server) should not flood a slow line, the resends making it worse.
pub const DEFAULT_RATE: u32 = 2 << 20;
/// How much of a second's worth may go at once after a quiet while.
const BURST: f64 = 0.1;

const TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE: Duration = Duration::from_millis(100);
const RESEND_MIN: Duration = Duration::from_millis(100);
const CONNECT_EVERY: Duration = Duration::from_millis(250);
const CONNECT_FOR: Duration = Duration::from_secs(10);
const FRAGMENTS_KEPT: Duration = Duration::from_secs(2);

/// Where packets go and come from: a UDP socket, or a network of tests.
pub trait Wire {
    fn send_to(&mut self, data: &[u8], to: SocketAddr) -> io::Result<()>;
    /// The next packet waiting, if any; never blocks.
    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
}

/// A UDP socket that does not block.
pub struct Udp(UdpSocket);

impl Udp {
    pub fn bind(addr: SocketAddr) -> io::Result<Udp> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        Ok(Udp(socket))
    }
}

impl Wire for Udp {
    fn send_to(&mut self, data: &[u8], to: SocketAddr) -> io::Result<()> {
        match self.0.send_to(data, to) {
            Ok(_) => Ok(()),
            // a full buffer loses the packet, as the network might
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        loop {
            match self.0.recv_from(buf) {
                Ok((n, from)) => return Ok(Some((n, from))),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                // Windows tells of a packet that could not be delivered earlier this way
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}

/// A number no one can guess beforehand.
fn random() -> u64 {
    use std::hash::{BuildHasher, RandomState};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    RandomState::new().hash_one((n, std::time::SystemTime::now()))
}

/// Whether sequence number `a` comes after `b`, the numbers going round.
fn newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

/// Reads a packet front to back; `None` when it is shorter than it says.
struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.data.len() < n {
            return None;
        }
        let (head, rest) = self.data.split_at(n);
        self.data = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
    }

    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.data)
    }
}

/// A piece of a reliable message.
#[derive(Debug, Clone)]
struct Chunk {
    id: u32,
    /// The message ends with it.
    last: bool,
    data: Vec<u8>,
    sent_at: Option<Instant>,
}

/// An unreliable message on its way in, in fragments.
struct Fragments {
    pieces: Vec<Option<Vec<u8>>>,
    since: Instant,
}

/// One side of a connection: what it sends and what it has received.
struct Peer {
    addr: SocketAddr,
    id: u64,
    /// The sequence number of the next packet sent.
    seq: u16,
    /// The newest packet received, and the numbers of those received lately by their place in
    /// a ring.
    newest: Option<u16>,
    seen: Vec<Option<u16>>,
    /// When the latest packets were sent, for the round trip.
    sent_at: VecDeque<(u16, Instant)>,
    /// Reliable chunks not yet acknowledged, oldest first.
    queue: VecDeque<Chunk>,
    next_chunk: u32,
    /// The next chunk to deliver, those after it that came early, and the message so far.
    expect: u32,
    early: BTreeMap<u32, (bool, Vec<u8>)>,
    assembling: Vec<u8>,
    unreliable: Vec<Vec<u8>>,
    group: u16,
    fragments: HashMap<u16, Fragments>,
    rtt: Duration,
    /// Bytes a second, and what may be sent now.
    rate: u32,
    budget: f64,
    budget_at: Option<Instant>,
    last_heard: Instant,
    last_sent: Option<Instant>,
    /// Something arrived that the other side should hear acknowledged.
    owe_ack: bool,
    inbox: VecDeque<Vec<u8>>,
    /// Why it must close, when something made it.
    broken: Option<String>,
}

impl Peer {
    fn new(addr: SocketAddr, id: u64, now: Instant) -> Peer {
        Peer {
            addr,
            id,
            seq: 0,
            newest: None,
            seen: vec![None; SEEN],
            sent_at: VecDeque::new(),
            queue: VecDeque::new(),
            next_chunk: 0,
            expect: 0,
            early: BTreeMap::new(),
            assembling: Vec::new(),
            unreliable: Vec::new(),
            group: 0,
            fragments: HashMap::new(),
            rtt: Duration::from_millis(100),
            rate: DEFAULT_RATE,
            budget: 0.0,
            budget_at: None,
            last_heard: now,
            last_sent: None,
            owe_ack: false,
            inbox: VecDeque::new(),
            broken: None,
        }
    }

    fn send(&mut self, data: &[u8], reliable: bool) -> Result<(), TooBig> {
        if reliable {
            if data.len() > MAX_RELIABLE {
                return Err(TooBig);
            }
            let mut pieces = data.chunks(CHUNK).peekable();
            if pieces.peek().is_none() {
                self.push_chunk(true, Vec::new());
            }
            while let Some(piece) = pieces.next() {
                self.push_chunk(pieces.peek().is_none(), piece.to_vec());
            }
        } else {
            if data.len() > MAX_UNRELIABLE {
                return Err(TooBig);
            }
            self.unreliable.push(data.to_vec());
        }
        Ok(())
    }

    fn pending(&self) -> usize {
        self.queue.iter().map(|c| c.data.len()).sum()
    }

    fn push_chunk(&mut self, last: bool, data: Vec<u8>) {
        let id = self.next_chunk;
        self.next_chunk = self.next_chunk.wrapping_add(1);
        self.queue.push_back(Chunk { id, last, data, sent_at: None });
    }

    /// A `DATA` packet's body arrived (after the connection's number).
    fn receive(&mut self, r: &mut Reader, now: Instant) {
        let (Some(seq), Some(ack), Some(received), Some(count)) = (r.u16(), r.u16(), r.u32(), r.u8()) else { return };
        let mut ranges = Vec::with_capacity(usize::from(count).min(RANGES));
        for _ in 0..count {
            let (Some(start), Some(len)) = (r.u32(), r.u16()) else { return };
            ranges.push((start, len));
        }
        // a packet seen already, or too old to tell, is dropped whole
        let slot = usize::from(seq) % SEEN;
        match self.newest {
            Some(newest) if !newer(seq, newest) && (usize::from(newest.wrapping_sub(seq)) >= SEEN || self.seen[slot] == Some(seq)) => {
                return;
            }
            Some(newest) if !newer(seq, newest) => {}
            _ => self.newest = Some(seq),
        }
        self.seen[slot] = Some(seq);
        self.last_heard = now;
        self.owe_ack = true;
        self.acknowledged(ack, received, &ranges, now);
        while let Some(kind) = r.u8() {
            let ok = match kind {
                RELIABLE => self.reliable(r),
                UNRELIABLE => r.u16().and_then(|len| r.take(len.into())).map(|data| self.inbox.push_back(data.to_vec())).is_some(),
                FRAGMENT => self.fragment(r, now),
                _ => false,
            };
            if !ok {
                break;
            }
        }
        self.fragments.retain(|_, f| now.duration_since(f.since) < FRAGMENTS_KEPT);
    }

    /// The other side has packet `ack`, every chunk before `received` and those in `ranges`:
    /// those are done with.
    fn acknowledged(&mut self, ack: u16, received: u32, ranges: &[(u32, u16)], now: Instant) {
        if let Some(i) = self.sent_at.iter().position(|&(seq, _)| seq == ack) {
            // a round trip, smoothed
            let sample = now.duration_since(self.sent_at[i].1);
            self.rtt = (self.rtt * 7 + sample) / 8;
            self.sent_at.drain(..=i);
        }
        let before = |id: u32| received.wrapping_sub(id).wrapping_sub(1) < 0x8000_0000;
        let within = |id: u32| ranges.iter().any(|&(start, len)| id.wrapping_sub(start) < u32::from(len));
        self.queue.retain(|c| !before(c.id) && !within(c.id));
    }

    fn reliable(&mut self, r: &mut Reader) -> bool {
        let (Some(id), Some(last), Some(len)) = (r.u32(), r.u8(), r.u16()) else { return false };
        let Some(data) = r.take(len.into()) else { return false };
        let ahead = id.wrapping_sub(self.expect);
        if ahead < WINDOW {
            self.early.entry(id).or_insert_with(|| (last != 0, data.to_vec()));
        }
        while let Some((last, data)) = self.early.remove(&self.expect) {
            self.expect = self.expect.wrapping_add(1);
            if self.assembling.len() + data.len() > MAX_RELIABLE {
                self.broken = Some("a message too big".into());
                return false;
            }
            self.assembling.extend_from_slice(&data);
            if last {
                self.inbox.push_back(std::mem::take(&mut self.assembling));
            }
        }
        true
    }

    fn fragment(&mut self, r: &mut Reader, now: Instant) -> bool {
        let (Some(group), Some(index), Some(count), Some(len)) = (r.u16(), r.u8(), r.u8(), r.u16()) else { return false };
        let Some(data) = r.take(len.into()) else { return false };
        if count == 0 || index >= count {
            return true;
        }
        let entry = self.fragments.entry(group).or_insert_with(|| Fragments { pieces: vec![None; count.into()], since: now });
        if entry.pieces.len() != usize::from(count) {
            return true;
        }
        entry.pieces[usize::from(index)] = Some(data.to_vec());
        if entry.pieces.iter().all(Option::is_some) {
            let whole = self.fragments.remove(&group).map(|f| f.pieces.into_iter().flatten().flatten().collect()).unwrap_or_default();
            self.inbox.push_back(whole);
        }
        true
    }

    fn header(&mut self, now: Instant) -> Vec<u8> {
        let mut p = Vec::with_capacity(MAX_PACKET);
        p.extend_from_slice(&MAGIC);
        p.push(DATA);
        p.extend_from_slice(&self.id.to_le_bytes());
        p.extend_from_slice(&self.seq.to_le_bytes());
        p.extend_from_slice(&self.newest.unwrap_or(0xffff).to_le_bytes());
        p.extend_from_slice(&self.expect.to_le_bytes());
        // the chunks that came early, as ranges
        let mut ranges: Vec<(u32, u16)> = Vec::new();
        for &id in self.early.keys() {
            if let Some((start, len)) = ranges.last_mut()
                && start.wrapping_add(u32::from(*len)) == id
                && *len < u16::MAX
            {
                *len += 1;
                continue;
            }
            if ranges.len() == RANGES {
                break;
            }
            ranges.push((id, 1));
        }
        p.push(ranges.len() as u8);
        for (start, len) in ranges {
            p.extend_from_slice(&start.to_le_bytes());
            p.extend_from_slice(&len.to_le_bytes());
        }
        self.sent_at.push_back((self.seq, now));
        if self.sent_at.len() > 256 {
            self.sent_at.pop_front();
        }
        self.seq = self.seq.wrapping_add(1);
        p
    }

    /// The packets to send now: chunks due, the unreliable messages, or an empty one to keep
    /// the other side hearing from us.
    fn flush(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let resend = RESEND_MIN.max(self.rtt * 3 / 2);
        let rate = f64::from(self.rate);
        let elapsed = self.budget_at.map_or(BURST, |at| now.duration_since(at).as_secs_f64());
        self.budget = (self.budget + rate * elapsed).min(rate * BURST + MAX_PACKET as f64);
        self.budget_at = Some(now);
        let mut packets: Vec<Vec<u8>> = Vec::new();
        let mut current: Option<Vec<u8>> = None;
        let mut put = |peer: &mut Peer, bytes: &[u8], packets: &mut Vec<Vec<u8>>| -> bool {
            if current.as_ref().is_some_and(|p| p.len() + bytes.len() > MAX_PACKET) {
                packets.extend(current.take());
            }
            if current.is_none() {
                if packets.len() >= PACKETS_PER_FLUSH {
                    return false;
                }
                current = Some(peer.header(now));
            }
            if let Some(p) = current.as_mut() {
                p.extend_from_slice(bytes);
            }
            true
        };
        // the reliable chunks due: never sent, or waiting longer than a round trip
        let due: Vec<usize> = (0..self.queue.len().min(IN_FLIGHT))
            .filter(|&i| self.queue[i].sent_at.is_none_or(|at| now.duration_since(at) >= resend))
            .collect();
        for i in due {
            if self.budget < 0.0 {
                break;
            }
            let c = &self.queue[i];
            let mut bytes = Vec::with_capacity(c.data.len() + 8);
            bytes.push(RELIABLE);
            bytes.extend_from_slice(&c.id.to_le_bytes());
            bytes.push(u8::from(c.last));
            bytes.extend_from_slice(&(c.data.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&c.data);
            if !put(self, &bytes, &mut packets) {
                break;
            }
            self.budget -= bytes.len() as f64;
            self.queue[i].sent_at = Some(now);
        }
        for message in std::mem::take(&mut self.unreliable) {
            if message.len() + 3 <= MAX_PACKET - DATA_HEADER {
                let mut bytes = vec![UNRELIABLE];
                bytes.extend_from_slice(&(message.len() as u16).to_le_bytes());
                bytes.extend_from_slice(&message);
                put(self, &bytes, &mut packets);
                self.budget -= bytes.len() as f64;
                continue;
            }
            let group = self.group;
            self.group = self.group.wrapping_add(1);
            let pieces: Vec<&[u8]> = message.chunks(FRAGMENT_BYTES).collect();
            for (index, piece) in pieces.iter().enumerate() {
                let mut bytes = vec![FRAGMENT];
                bytes.extend_from_slice(&group.to_le_bytes());
                bytes.push(index as u8);
                bytes.push(pieces.len() as u8);
                bytes.extend_from_slice(&(piece.len() as u16).to_le_bytes());
                bytes.extend_from_slice(piece);
                put(self, &bytes, &mut packets);
                self.budget -= bytes.len() as f64;
            }
        }
        packets.extend(current.take());
        let quiet = self.last_sent.is_none_or(|at| now.duration_since(at) >= KEEPALIVE);
        if packets.is_empty() && (self.owe_ack || quiet) {
            packets.push(self.header(now));
        }
        if !packets.is_empty() {
            self.owe_ack = false;
            self.last_sent = Some(now);
        }
        packets
    }
}

/// A message too big to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooBig;

impl std::fmt::Display for TooBig {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("message too big")
    }
}

impl std::error::Error for TooBig {}

fn control(kind: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut p = MAGIC.to_vec();
    p.push(kind);
    for part in parts {
        p.extend_from_slice(part);
    }
    p
}

/// A client of a server: its connection's number.
pub type ClientId = u64;

/// What happened on a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// Someone asks to connect, saying `hello`: answer with [`Server::accept`] or
    /// [`Server::deny`].
    Request {
        addr: SocketAddr,
        hello: Vec<u8>,
    },
    Message {
        client: ClientId,
        data: Vec<u8>,
    },
    Disconnected {
        client: ClientId,
        reason: String,
    },
}

/// Someone who asked to connect and has not been answered, or was answered and may ask again.
struct Asked {
    addr: SocketAddr,
    salt: u64,
    /// What they were answered: the connection made, or why not.
    answer: Option<Result<(ClientId, Vec<u8>), String>>,
}

/// A server: it listens for clients and keeps a connection with each it accepts.
pub struct Server<W: Wire = Udp> {
    wire: W,
    peers: Vec<Peer>,
    asked: Vec<Asked>,
    events: Vec<ServerEvent>,
    rate: u32,
}

impl Server<Udp> {
    /// A server listening at `addr` (`0.0.0.0:27015`).
    pub fn bind(addr: SocketAddr) -> io::Result<Server<Udp>> {
        Ok(Server::with_wire(Udp::bind(addr)?))
    }
}

impl<W: Wire> Server<W> {
    pub fn with_wire(wire: W) -> Server<W> {
        Server { wire, peers: Vec::new(), asked: Vec::new(), events: Vec::new(), rate: DEFAULT_RATE }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.wire.local_addr()
    }

    /// Reads what arrived and tells what happened.
    pub fn poll(&mut self, now: Instant) -> Vec<ServerEvent> {
        let mut buf = [0u8; 2048];
        while let Ok(Some((n, from))) = self.wire.recv_from(&mut buf) {
            let packet = buf[..n.min(buf.len())].to_vec();
            self.packet(&packet, from, now);
        }
        // the clients who went quiet, or broke the rules
        let mut gone = Vec::new();
        for peer in &mut self.peers {
            if let Some(reason) = peer.broken.take() {
                gone.push((peer.id, reason));
            } else if now.duration_since(peer.last_heard) > TIMEOUT {
                gone.push((peer.id, "timed out".to_string()));
            }
        }
        for (client, reason) in gone {
            self.disconnect(client, &reason);
            self.events.push(ServerEvent::Disconnected { client, reason });
        }
        for peer in &mut self.peers {
            while let Some(data) = peer.inbox.pop_front() {
                self.events.push(ServerEvent::Message { client: peer.id, data });
            }
        }
        std::mem::take(&mut self.events)
    }

    fn packet(&mut self, packet: &[u8], from: SocketAddr, now: Instant) {
        let mut r = Reader { data: packet };
        if r.take(4) != Some(&MAGIC[..]) {
            return;
        }
        match r.u8() {
            Some(CONNECT) => {
                let (Some(version), Some(salt)) = (r.u16(), r.u64()) else { return };
                let hello = r.rest().to_vec();
                if let Some(asked) = self.asked.iter().find(|a| a.addr == from && a.salt == salt) {
                    // asked again: the answer was lost, or is not made yet
                    match &asked.answer {
                        Some(Ok((id, welcome))) => {
                            let reply = control(ACCEPT, &[&salt.to_le_bytes(), &id.to_le_bytes(), welcome]);
                            let _ = self.wire.send_to(&reply, from);
                        }
                        Some(Err(reason)) => {
                            let _ = self.wire.send_to(&control(DENY, &[&salt.to_le_bytes(), reason.as_bytes()]), from);
                        }
                        None => {}
                    }
                    return;
                }
                if version != PROTOCOL {
                    let reason = format!("the server speaks protocol {PROTOCOL}, the client {version}");
                    let _ = self.wire.send_to(&control(DENY, &[&salt.to_le_bytes(), reason.as_bytes()]), from);
                    return;
                }
                // a new attempt from the same place replaces the old one
                self.asked.retain(|a| a.addr != from);
                if self.asked.len() >= 256 {
                    self.asked.remove(0);
                }
                self.asked.push(Asked { addr: from, salt, answer: None });
                self.events.push(ServerEvent::Request { addr: from, hello });
            }
            Some(DATA) => {
                let Some(id) = r.u64() else { return };
                if let Some(peer) = self.peers.iter_mut().find(|p| p.id == id && p.addr == from) {
                    peer.receive(&mut r, now);
                }
            }
            Some(DISCONNECT) => {
                let Some(id) = r.u64() else { return };
                if let Some(i) = self.peers.iter().position(|p| p.id == id && p.addr == from) {
                    let reason = String::from_utf8_lossy(r.rest()).into_owned();
                    self.peers.remove(i);
                    self.asked.retain(|a| a.addr != from);
                    self.events.push(ServerEvent::Disconnected { client: id, reason });
                }
            }
            _ => {}
        }
    }

    /// Lets in someone who asked, answering `welcome`: their connection, or none if they did
    /// not ask (or no longer wait) - or if the welcome is longer than [`MAX_HELLO`], when they
    /// are turned away.
    pub fn accept(&mut self, addr: SocketAddr, welcome: &[u8], now: Instant) -> Option<ClientId> {
        if welcome.len() > MAX_HELLO {
            self.deny(addr, "the server's welcome is too long");
            return None;
        }
        let asked = self.asked.iter_mut().find(|a| a.addr == addr && a.answer.is_none())?;
        let id = random();
        asked.answer = Some(Ok((id, welcome.to_vec())));
        let reply = control(ACCEPT, &[&asked.salt.to_le_bytes(), &id.to_le_bytes(), welcome]);
        let _ = self.wire.send_to(&reply, addr);
        // someone coming back from the same address: their old connection is gone
        if let Some(i) = self.peers.iter().position(|p| p.addr == addr) {
            let old = self.peers.remove(i);
            self.events.push(ServerEvent::Disconnected { client: old.id, reason: "reconnected".to_string() });
        }
        let mut peer = Peer::new(addr, id, now);
        peer.rate = self.rate;
        self.peers.push(peer);
        Some(id)
    }

    /// Turns away someone who asked, saying why.
    pub fn deny(&mut self, addr: SocketAddr, reason: &str) {
        let Some(asked) = self.asked.iter_mut().find(|a| a.addr == addr && a.answer.is_none()) else { return };
        asked.answer = Some(Err(reason.to_string()));
        let _ = self.wire.send_to(&control(DENY, &[&asked.salt.to_le_bytes(), reason.as_bytes()]), addr);
    }

    /// Sends a client a message, reliable or not.
    pub fn send(&mut self, client: ClientId, data: &[u8], reliable: bool) -> Result<(), TooBig> {
        match self.peers.iter_mut().find(|p| p.id == client) {
            Some(peer) => peer.send(data, reliable),
            None => Ok(()),
        }
    }

    /// Sends what is due to every client.
    pub fn flush(&mut self, now: Instant) {
        for peer in &mut self.peers {
            for packet in peer.flush(now) {
                let _ = self.wire.send_to(&packet, peer.addr);
            }
        }
    }

    /// Closes a client's connection, telling them why.
    pub fn disconnect(&mut self, client: ClientId, reason: &str) {
        let Some(i) = self.peers.iter().position(|p| p.id == client) else { return };
        let peer = self.peers.remove(i);
        let bye = control(DISCONNECT, &[&peer.id.to_le_bytes(), reason.as_bytes()]);
        for _ in 0..3 {
            let _ = self.wire.send_to(&bye, peer.addr);
        }
        self.asked.retain(|a| a.addr != peer.addr);
    }

    /// The clients connected.
    pub fn clients(&self) -> Vec<ClientId> {
        self.peers.iter().map(|p| p.id).collect()
    }

    /// Where a client is, and how long a packet takes there and back.
    pub fn client_info(&self, client: ClientId) -> Option<(SocketAddr, Duration)> {
        self.peers.iter().find(|p| p.id == client).map(|p| (p.addr, p.rtt))
    }

    /// How many bytes of reliable messages to a client are not yet acknowledged.
    pub fn pending(&self, client: ClientId) -> usize {
        self.peers.iter().find(|p| p.id == client).map_or(0, Peer::pending)
    }

    /// How fast reliable messages go to each client, in bytes a second.
    pub fn set_rate(&mut self, bytes_per_second: u32) {
        self.rate = bytes_per_second.max(MAX_PACKET as u32);
        for peer in &mut self.peers {
            peer.rate = self.rate;
        }
    }
}

/// What happened on a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientEvent {
    /// The server let us in, saying `welcome`.
    Accepted {
        welcome: Vec<u8>,
    },
    Denied {
        reason: String,
    },
    Message {
        data: Vec<u8>,
    },
    Disconnected {
        reason: String,
    },
}

enum State {
    Connecting { since: Instant, tried: Option<Instant> },
    Connected(Box<Peer>),
    Closed,
}

/// A client: one connection to a server.
pub struct Client<W: Wire = Udp> {
    wire: W,
    server: SocketAddr,
    salt: u64,
    hello: Vec<u8>,
    state: State,
    rate: u32,
}

impl Client<Udp> {
    /// Starts connecting to a server, saying `hello`.
    pub fn connect(server: SocketAddr, hello: &[u8], now: Instant) -> io::Result<Client<Udp>> {
        if hello.len() > MAX_HELLO {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the hello is too long"));
        }
        let any: SocketAddr = if server.is_ipv4() { "0.0.0.0:0".parse() } else { "[::]:0".parse() }.map_err(io::Error::other)?;
        Ok(Client::with_wire(Udp::bind(any)?, server, hello, now))
    }
}

impl<W: Wire> Client<W> {
    pub fn with_wire(wire: W, server: SocketAddr, hello: &[u8], now: Instant) -> Client<W> {
        debug_assert!(hello.len() <= MAX_HELLO, "a hello of {} bytes", hello.len());
        Client {
            wire,
            server,
            salt: random(),
            hello: hello.to_vec(),
            state: State::Connecting { since: now, tried: None },
            rate: DEFAULT_RATE,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected(_))
    }

    /// How long a packet takes to the server and back.
    pub fn rtt(&self) -> Option<Duration> {
        match &self.state {
            State::Connected(peer) => Some(peer.rtt),
            _ => None,
        }
    }

    /// How many bytes of reliable messages to the server are not yet acknowledged.
    pub fn pending(&self) -> usize {
        match &self.state {
            State::Connected(peer) => peer.pending(),
            _ => 0,
        }
    }

    /// How fast reliable messages go to the server, in bytes a second.
    pub fn set_rate(&mut self, bytes_per_second: u32) {
        self.rate = bytes_per_second.max(MAX_PACKET as u32);
        if let State::Connected(peer) = &mut self.state {
            peer.rate = self.rate;
        }
    }

    /// Reads what arrived and tells what happened.
    pub fn poll(&mut self, now: Instant) -> Vec<ClientEvent> {
        let mut events = Vec::new();
        let mut buf = [0u8; 2048];
        while let Ok(Some((n, from))) = self.wire.recv_from(&mut buf) {
            if from != self.server {
                continue;
            }
            let mut r = Reader { data: &buf[..n.min(buf.len())] };
            if r.take(4) != Some(&MAGIC[..]) {
                continue;
            }
            match (r.u8(), &mut self.state) {
                (Some(ACCEPT), State::Connecting { .. }) => {
                    let (Some(salt), Some(id)) = (r.u64(), r.u64()) else { continue };
                    if salt == self.salt {
                        let mut peer = Peer::new(self.server, id, now);
                        peer.rate = self.rate;
                        self.state = State::Connected(Box::new(peer));
                        events.push(ClientEvent::Accepted { welcome: r.rest().to_vec() });
                    }
                }
                (Some(DENY), State::Connecting { .. }) => {
                    if r.u64() == Some(self.salt) {
                        self.state = State::Closed;
                        events.push(ClientEvent::Denied { reason: String::from_utf8_lossy(r.rest()).into_owned() });
                    }
                }
                (Some(DATA), State::Connected(peer)) => {
                    if r.u64() == Some(peer.id) {
                        peer.receive(&mut r, now);
                    }
                }
                (Some(DISCONNECT), State::Connected(peer)) if r.u64() == Some(peer.id) => {
                    let reason = String::from_utf8_lossy(r.rest()).into_owned();
                    self.state = State::Closed;
                    events.push(ClientEvent::Disconnected { reason });
                }
                _ => {}
            }
        }
        match &mut self.state {
            State::Connecting { since, .. } if now.duration_since(*since) > CONNECT_FOR => {
                self.state = State::Closed;
                events.push(ClientEvent::Denied { reason: "the server did not answer".into() });
            }
            State::Connected(peer) => {
                while let Some(data) = peer.inbox.pop_front() {
                    events.push(ClientEvent::Message { data });
                }
                let reason =
                    peer.broken.take().or_else(|| (now.duration_since(peer.last_heard) > TIMEOUT).then(|| "timed out".to_string()));
                if let Some(reason) = reason {
                    self.disconnect(&reason);
                    events.push(ClientEvent::Disconnected { reason });
                }
            }
            _ => {}
        }
        events
    }

    /// Sends the server a message, reliable or not; nothing is sent before the connection is
    /// made.
    pub fn send(&mut self, data: &[u8], reliable: bool) -> Result<(), TooBig> {
        match &mut self.state {
            State::Connected(peer) => peer.send(data, reliable),
            _ => Ok(()),
        }
    }

    /// Sends what is due: asking to connect again, or the connection's packets.
    pub fn flush(&mut self, now: Instant) {
        match &mut self.state {
            State::Connecting { tried, .. } => {
                if tried.is_none_or(|at| now.duration_since(at) >= CONNECT_EVERY) {
                    *tried = Some(now);
                    let ask = control(CONNECT, &[&PROTOCOL.to_le_bytes(), &self.salt.to_le_bytes(), &self.hello]);
                    let _ = self.wire.send_to(&ask, self.server);
                }
            }
            State::Connected(peer) => {
                for packet in peer.flush(now) {
                    let _ = self.wire.send_to(&packet, self.server);
                }
            }
            State::Closed => {}
        }
    }

    /// Leaves the server, telling it why.
    pub fn disconnect(&mut self, reason: &str) {
        if let State::Connected(peer) = &self.state {
            let bye = control(DISCONNECT, &[&peer.id.to_le_bytes(), reason.as_bytes()]);
            for _ in 0..3 {
                let _ = self.wire.send_to(&bye, self.server);
            }
        }
        self.state = State::Closed;
    }
}

#[cfg(test)]
mod tests;
