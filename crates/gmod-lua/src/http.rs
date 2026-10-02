//! `HTTP`: requests from Lua, made in the background and answered on the game's thread.
//!
//! `HTTP(request)` queues a request and returns at once; a thread of its own makes it (at
//! most [`MAX_PARALLEL`] at a time, the others wait their turn), and
//! [`crate::Runtime::think`] hands the answer to the request's `success(code, body, headers)`
//! or `failed(reason)`, as the engine does on a later frame. `http.Fetch` and `http.Post` are
//! the game's own Lua on top (`includes/modules/http.lua`).
//!
//! As in GMod, a request never reaches a private network - the player's router, a service on
//! their machine - whatever a script, a server or a redirect asks for. The check is made on
//! the addresses the name resolves to, at every connection, so a name that points at a private
//! address is refused too. A dedicated server can allow it, as GMod's `-allowlocalhttp`:
//! [`crate::Runtime::allow_local_http`]. Requests go out directly, never through a proxy of
//! the system, so the check is made against the real destination.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use mlua::{Function, Lua, MultiValue, Table, Value};
use ureq::config::Config;
use ureq::http;
use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use crate::{ScriptError, files, host};

/// What GMod waits for an answer when the request does not say.
const DEFAULT_TIMEOUT: f64 = 60.0;
/// The largest body taken in; a larger one fails, as large bodies do in GMod.
const MAX_BODY: u64 = 64 << 20;
/// Requests made at the same time; a script that asks for hundreds at once gets them in turn.
pub const MAX_PARALLEL: usize = 16;

/// What a request ended with, sent back from its thread.
enum Outcome {
    Success { code: u16, body: Vec<u8>, headers: Vec<(String, String)> },
    Failed(&'static str),
}

/// `success` and `failed` of a request.
type Callbacks = (Option<Function>, Option<Function>);

/// An answer: the request, how it ended, and whether a thread made it.
type Answer = (u64, Outcome, bool);

/// The requests in flight and the way their answers come back.
pub(crate) struct Http {
    sender: Sender<Answer>,
    receiver: Receiver<Answer>,
    /// The callbacks of each request; they stay on the game's thread.
    pending: RefCell<HashMap<u64, Callbacks>>,
    next_id: Cell<u64>,
    /// Threads making a request now, and the requests waiting for one.
    running: Cell<usize>,
    queued: RefCell<VecDeque<(u64, Request, ureq::Agent)>>,
    pub(crate) allow_local: Cell<bool>,
    agents: RefCell<Option<(ureq::Agent, ureq::Agent)>>,
}

impl Default for Http {
    fn default() -> Http {
        let (sender, receiver) = channel();
        Http {
            sender,
            receiver,
            pending: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
            running: Cell::new(0),
            queued: RefCell::new(VecDeque::new()),
            allow_local: Cell::new(false),
            agents: RefCell::new(None),
        }
    }
}

impl Http {
    /// The agent for requests: one that stays off private networks, or one that does not.
    fn agent(&self) -> ureq::Agent {
        let mut agents = self.agents.borrow_mut();
        let (public, any) = agents.get_or_insert_with(|| (agent(false), agent(true)));
        if self.allow_local.get() { any.clone() } else { public.clone() }
    }

    /// Makes the request on a thread of its own, or queues it when enough are running.
    fn start(&self, id: u64, request: Request, agent: ureq::Agent) {
        if self.running.get() >= MAX_PARALLEL {
            self.queued.borrow_mut().push_back((id, request, agent));
            return;
        }
        let sender = self.sender.clone();
        let spawned = std::thread::Builder::new().name("http".into()).spawn(move || {
            let outcome = perform(&agent, request);
            let _ = sender.send((id, outcome, true));
        });
        match spawned {
            Ok(_) => self.running.set(self.running.get() + 1),
            Err(_) => self.answer_now(id, "unsuccessful"),
        }
    }

    /// A failure known at once; the callback still runs on a later frame, as for the others.
    fn answer_now(&self, id: u64, reason: &'static str) {
        let _ = self.sender.send((id, Outcome::Failed(reason), false));
    }

    /// How many requests have not been answered yet.
    pub(crate) fn in_flight(&self) -> usize {
        self.pending.borrow().len()
    }
}

fn agent(allow_local: bool) -> ureq::Agent {
    let config = Config::builder()
        // every status is an answer for Lua, 404 as much as 200
        .http_status_as_error(false)
        .proxy(None)
        .user_agent(concat!("openGMOD/", env!("CARGO_PKG_VERSION")))
        // the system's roots and checks: an antivirus that scans HTTPS puts its root there
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .build();
    ureq::Agent::with_parts(config, DefaultConnector::default(), PublicResolver { allow_local })
}

/// The system's resolver, keeping only the addresses a request may go to.
#[derive(Debug)]
struct PublicResolver {
    allow_local: bool,
}

impl Resolver for PublicResolver {
    fn resolve(&self, uri: &http::Uri, config: &Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let all = DefaultResolver::default().resolve(uri, config, timeout)?;
        if self.allow_local {
            return Ok(all);
        }
        let mut kept = self.empty();
        for address in all.iter().filter(|a| is_public(a.ip())) {
            kept.push(*address);
        }
        if kept.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "requests to private networks are not allowed",
            )));
        }
        Ok(kept)
    }
}

/// Whether an address is on the public internet: not this machine, not a private or
/// link-local network, not a range set aside for something else.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0 // "this network"
        || a == 10
        || a == 127
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 0 && c == 0) // protocol assignments
        || (a == 192 && b == 0 && c == 2) // documentation
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19)) // benchmarking
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224) // multicast, reserved, broadcast
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    // NAT64 (64:ff9b::/96) and 6to4 (2002::/16) carry an IPv4 address inside
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0; 4] {
        return is_public_v4(Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8));
    }
    if s[0] == 0x2002 {
        return is_public_v4(Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || s[0..6] == [0; 6] // IPv4-compatible, deprecated
        || (s[0] & 0xfe00) == 0xfc00 // unique local
        || (s[0] & 0xffc0) == 0xfe80 // link-local
        || (s[0] & 0xffc0) == 0xfec0 // site-local, deprecated
        || (s[0] & 0xff00) == 0xff00 // multicast
        || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
        || (s[0] == 0x0100 && s[1..4] == [0; 3])) // discard
}

/// `%`-encoding for a query string or a form body.
fn encode(text: &[u8]) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `a=1&b=2` from a table of parameters, in the order of the table.
fn form(lua: &Lua, parameters: &Table) -> mlua::Result<String> {
    let mut pairs = Vec::new();
    for pair in parameters.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let (Some(key), Some(value)) = (lua.coerce_string(key)?, lua.coerce_string(value)?) else { continue };
        pairs.push(format!("{}={}", encode(&key.as_bytes()), encode(&value.as_bytes())));
    }
    Ok(pairs.join("&"))
}

/// `content-type` as servers write it: `Content-Type`. Names come lower case from the HTTP
/// library; scripts look them up as the server sent them, which is nearly always this.
fn header_case(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map(|first| first.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// A request ready to go to its thread: everything taken out of Lua.
struct Request {
    method: http::Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<(Vec<u8>, String)>,
    timeout: Duration,
}

/// What the request table asks for; `Err` with the reason it fails at once.
fn read_request(lua: &Lua, t: &Table) -> mlua::Result<Result<Request, &'static str>> {
    let method = t.get::<Option<String>>("method")?.unwrap_or_else(|| "GET".into()).to_ascii_uppercase();
    let method = match method.as_str() {
        "GET" | "POST" | "HEAD" | "PUT" | "DELETE" | "PATCH" | "OPTIONS" => {
            http::Method::from_bytes(method.as_bytes()).expect("a standard method")
        }
        _ => return Ok(Err("unsuccessful")),
    };
    let Some(mut url) = t.get::<Option<String>>("url")? else { return Ok(Err("invalid url")) };
    let scheme = url.get(..8).unwrap_or(&url).to_ascii_lowercase();
    if !(scheme.starts_with("http://") || scheme.starts_with("https://")) || url.parse::<http::Uri>().is_err() {
        return Ok(Err("invalid url"));
    }

    let mut headers = Vec::new();
    if let Some(table) = t.get::<Option<Table>>("headers")? {
        for pair in table.pairs::<Value, Value>() {
            let (key, value) = pair?;
            if let (Some(key), Some(value)) = (lua.coerce_string(key)?, lua.coerce_string(value)?) {
                headers.push((key.to_string_lossy(), value.to_string_lossy()));
            }
        }
    }

    let parameters = match t.get::<Option<Table>>("parameters")? {
        Some(table) => Some(form(lua, &table)?),
        None => None,
    };
    let body = t.get::<Option<mlua::LuaString>>("body")?.map(|b| b.as_bytes().to_vec());
    let content_type = t.get::<Option<String>>("type")?.unwrap_or_else(|| "text/plain; charset=utf-8".into());
    let body = match method {
        // parameters go into the address
        http::Method::GET | http::Method::HEAD => {
            if let Some(query) = parameters.filter(|q| !q.is_empty()) {
                url.push(if url.contains('?') { '&' } else { '?' });
                url.push_str(&query);
            }
            None
        }
        // a body wins over parameters, which are sent as a form otherwise
        http::Method::POST => match (body, parameters) {
            (Some(body), _) => Some((body, content_type)),
            (None, Some(query)) => Some((query.into_bytes(), "application/x-www-form-urlencoded".into())),
            (None, None) => None,
        },
        _ => body.map(|body| (body, content_type)),
    };
    let timeout = t.get::<Option<f64>>("timeout")?.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(DEFAULT_TIMEOUT);
    Ok(Ok(Request { method, url, headers, body, timeout: Duration::from_secs_f64(timeout) }))
}

/// Makes the request, on its own thread.
fn perform(agent: &ureq::Agent, request: Request) -> Outcome {
    let Request { method, url, headers, body, timeout } = request;
    let mut builder = http::Request::builder().method(method).uri(&url);
    for (name, value) in &headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    let sent = match body {
        Some((body, content_type)) => {
            let has_type = headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
            if !has_type {
                builder = builder.header("Content-Type", content_type);
            }
            builder.body(body).map_err(|_| "invalid request").and_then(|r| run(agent, r, timeout))
        }
        None => builder.body(()).map_err(|_| "invalid request").and_then(|r| run(agent, r, timeout)),
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(reason) => return Outcome::Failed(reason),
    };
    let code = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| (header_case(name.as_str()), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    match response.body_mut().with_config().limit(MAX_BODY).read_to_vec() {
        Ok(body) => Outcome::Success { code, body, headers },
        Err(_) => Outcome::Failed("unsuccessful"),
    }
}

fn run(
    agent: &ureq::Agent,
    request: http::Request<impl ureq::AsSendBody>,
    timeout: Duration,
) -> Result<http::Response<ureq::Body>, &'static str> {
    use ureq::RequestExt as _;
    request.with_agent(agent).configure().timeout_global(Some(timeout)).run().map_err(|e| match e {
        ureq::Error::Timeout(_) => "timeout",
        ureq::Error::BadUri(_) => "invalid url",
        _ => "unsuccessful",
    })
}

/// Hands the answers that came in to their callbacks.
pub(crate) fn deliver(lua: &Lua) {
    loop {
        let (outcome, callbacks) = {
            let host = host(lua);
            let http = &host.http;
            let Ok((id, outcome, from_thread)) = http.receiver.try_recv() else { return };
            if from_thread {
                http.running.set(http.running.get() - 1);
                let next = http.queued.borrow_mut().pop_front();
                if let Some((id, request, agent)) = next {
                    http.start(id, request, agent);
                }
            }
            let callbacks = http.pending.borrow_mut().remove(&id);
            (outcome, callbacks)
        };
        let Some((success, failed)) = callbacks else { continue };
        let call = || -> mlua::Result<Option<(Function, MultiValue)>> {
            Ok(match outcome {
                Outcome::Success { code, body, headers } => match success {
                    Some(f) => {
                        let table = lua.create_table()?;
                        for (name, value) in headers {
                            table.raw_set(name, value)?;
                        }
                        let args = (code, lua.create_string(body)?, table);
                        Some((f, mlua::IntoLuaMulti::into_lua_multi(args, lua)?))
                    }
                    None => None,
                },
                Outcome::Failed(reason) => match failed {
                    Some(f) => Some((f, mlua::IntoLuaMulti::into_lua_multi(reason, lua)?)),
                    None => None,
                },
            })
        };
        match call() {
            Ok(Some((f, args))) => {
                let _ = files::call_reporting(lua, f, args);
            }
            Ok(None) => {}
            Err(e) => host(lua).report(ScriptError::from_lua(&e)),
        }
    }
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().raw_set(
        "HTTP",
        lua.create_function(|lua, value: Value| {
            let Value::Table(t) = value else { return Ok(false) };
            let request = read_request(lua, &t)?;
            let success = t.get::<Option<Function>>("success")?;
            let failed = t.get::<Option<Function>>("failed")?;
            let host = host(lua);
            let id = host.http.next_id.get();
            host.http.next_id.set(id + 1);
            host.http.pending.borrow_mut().insert(id, (success, failed));
            match request {
                Ok(request) => host.http.start(id, request, host.http.agent()),
                Err(reason) => host.http.answer_now(id, reason),
            }
            Ok(true)
        })?,
    )
}
