//! A master server: the list of the servers on the internet, for the menu's server browser.
//!
//! A server announces itself (`HEARTBEAT`, padded to a whole packet, every minute); the master
//! asks it what it is (`QUERY`) at the address the announcement came from, and lists it once
//! it answers (`INFO`) - so only a server that is there is listed, at the address it is
//! reached at, and a forged sender address gets a question no longer than its own packet. A
//! server that has not announced itself for five minutes goes. A browser asks for the list
//! (`LIST`, padded too) a page at a time (`SERVERS`), then asks each server itself, as it asks
//! those of the local network. Nothing more is kept or said: the master never vouches for a
//! server, whose key the player learns as it joins ([`crate::crypto`]).

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use crate::{HEARTBEAT, INFO, LIST, MAGIC, MAX_PACKET, QUERY, Reader, SERVERS, Udp, Wire, control, random};

/// The port masters listen at unless they say otherwise.
pub const MASTER_PORT: u16 = 27011;
/// How long a server stays listed after it last announced itself.
const LISTED_FOR: Duration = Duration::from_secs(300);
/// How long the master waits for a server to answer its question.
const CHECK_FOR: Duration = Duration::from_secs(5);
/// The most servers listed, and questions out at once.
const MAX_SERVERS: usize = 65536;
const MAX_CHECKING: usize = 4096;
/// Where a page's addresses start: after the magic, the kind, the question, how many there are
/// in all and the first one's place.
const PAGE_HEADER: usize = 4 + 1 + 8 + 4 + 4;

/// An address as a page has it: 4 or 6, the address, the port.
pub(crate) fn write_address(out: &mut Vec<u8>, addr: SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_le_bytes());
}

pub(crate) fn read_address(r: &mut Reader) -> Option<SocketAddr> {
    let ip = match r.u8()? {
        4 => IpAddr::from(<[u8; 4]>::try_from(r.take(4)?).ok()?),
        6 => IpAddr::from(<[u8; 16]>::try_from(r.take(16)?).ok()?),
        _ => return None,
    };
    Some(SocketAddr::new(ip, r.u16()?))
}

/// A server listed: where, its protocol, when it last announced itself.
struct Listed {
    addr: SocketAddr,
    protocol: u16,
    seen: Instant,
}

/// A master server.
pub struct Master<W: Wire = Udp> {
    wire: W,
    servers: Vec<Listed>,
    /// The questions out to those who announced themselves, by number: where, their protocol,
    /// when asked.
    checking: HashMap<u64, (SocketAddr, u16, Instant)>,
}

impl Master<Udp> {
    /// A master listening at `addr` (`0.0.0.0:27011`).
    pub fn bind(addr: SocketAddr) -> io::Result<Master<Udp>> {
        Ok(Master::with_wire(Udp::bind(addr)?))
    }
}

impl<W: Wire> Master<W> {
    pub fn with_wire(wire: W) -> Master<W> {
        Master { wire, servers: Vec::new(), checking: HashMap::new() }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.wire.local_addr()
    }

    /// The servers listed now.
    pub fn servers(&self) -> Vec<SocketAddr> {
        self.servers.iter().map(|s| s.addr).collect()
    }

    /// Reads what arrived and answers it.
    pub fn poll(&mut self, now: Instant) {
        let mut buf = [0u8; 2048];
        while let Ok(Some((n, from))) = self.wire.recv_from(&mut buf) {
            let packet = &buf[..n.min(buf.len())];
            let mut r = Reader { data: packet };
            if r.take(4) != Some(&MAGIC[..]) {
                continue;
            }
            match r.u8() {
                // a request shorter than its answer would make the master an amplifier
                Some(HEARTBEAT) if packet.len() >= MAX_PACKET => {
                    let Some(protocol) = r.u16() else { continue };
                    if self.checking.len() >= MAX_CHECKING {
                        continue;
                    }
                    let question = random();
                    let mut ask = control(QUERY, &[&question.to_le_bytes()]);
                    ask.resize(MAX_PACKET, 0);
                    let _ = self.wire.send_to(&ask, from);
                    self.checking.insert(question, (from, protocol, now));
                }
                Some(INFO) => {
                    let Some(question) = r.u64() else { continue };
                    let Some(&(addr, protocol, _)) = self.checking.get(&question).filter(|(addr, ..)| *addr == from) else { continue };
                    self.checking.remove(&question);
                    if let Some(listed) = self.servers.iter_mut().find(|s| s.addr == addr) {
                        listed.protocol = protocol;
                        listed.seen = now;
                    } else if self.servers.len() < MAX_SERVERS {
                        self.servers.push(Listed { addr, protocol, seen: now });
                    }
                }
                Some(LIST) if packet.len() >= MAX_PACKET => {
                    let (Some(question), Some(protocol), Some(start)) = (r.u64(), r.u16(), r.u32()) else { continue };
                    let page = self.page(question, protocol, start as usize);
                    let _ = self.wire.send_to(&page, from);
                }
                _ => {}
            }
        }
        self.servers.retain(|s| now.duration_since(s.seen) < LISTED_FOR);
        self.checking.retain(|_, (_, _, at)| now.duration_since(*at) < CHECK_FOR);
    }

    /// The page of the servers of a protocol from the `start`-th: as many as fit a packet.
    fn page(&self, question: u64, protocol: u16, start: usize) -> Vec<u8> {
        let all: Vec<SocketAddr> = self.servers.iter().filter(|s| s.protocol == protocol).map(|s| s.addr).collect();
        let mut entries = Vec::new();
        for addr in all.iter().skip(start) {
            let mut entry = Vec::with_capacity(19);
            write_address(&mut entry, *addr);
            if PAGE_HEADER + entries.len() + entry.len() > MAX_PACKET {
                break;
            }
            entries.extend(entry);
        }
        control(SERVERS, &[&question.to_le_bytes(), &(all.len() as u32).to_le_bytes(), &(start as u32).to_le_bytes(), &entries])
    }
}
