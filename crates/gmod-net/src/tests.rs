use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::master::Master;

/// A network for tests: packets wait a few steps, some are lost, some come twice, some
/// overtake others - as the dice say, the same every run.
#[derive(Default)]
struct Net {
    /// (when it arrives, to, from, packet)
    flying: Vec<(u64, SocketAddr, SocketAddr, Vec<u8>)>,
    step: u64,
    dice: u64,
    loss: u64,
    duplicate: u64,
    delay: u64,
}

impl Net {
    fn roll(&mut self) -> u64 {
        // xorshift
        let mut x = self.dice.max(1);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.dice = x;
        x % 100
    }
}

#[derive(Clone)]
struct TestWire {
    net: Rc<RefCell<Net>>,
    addr: SocketAddr,
}

impl Wire for TestWire {
    fn send_to(&mut self, data: &[u8], to: SocketAddr) -> io::Result<()> {
        let mut net = self.net.borrow_mut();
        if net.roll() < net.loss {
            return Ok(());
        }
        let copies = if net.roll() < net.duplicate { 2 } else { 1 };
        for _ in 0..copies {
            let delay = 1 + if net.delay > 0 { net.roll() % net.delay } else { 0 };
            let at = net.step + delay;
            net.flying.push((at, to, self.addr, data.to_vec()));
        }
        Ok(())
    }

    fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        let mut net = self.net.borrow_mut();
        let step = net.step;
        let Some(i) = net.flying.iter().position(|(at, to, _, _)| *at <= step && *to == self.addr) else { return Ok(None) };
        let (_, _, from, data) = net.flying.remove(i);
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(Some((n, from)))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, 1], port))
}

/// A server and a client on one test network, connected (the server accepting with
/// `welcome`), and the clock.
struct Pair {
    net: Rc<RefCell<Net>>,
    server: Server<TestWire>,
    client: Client<TestWire>,
    /// Who the client is.
    alice: Identity,
    id: ClientId,
    now: Instant,
    start: Instant,
    server_got: Vec<Vec<u8>>,
    client_got: Vec<Vec<u8>>,
}

impl Pair {
    fn new(loss: u64, duplicate: u64, delay: u64) -> Pair {
        let net = Rc::new(RefCell::new(Net { dice: 12345, loss, duplicate, delay, ..Net::default() }));
        let start = Instant::now();
        let server = Server::with_wire(TestWire { net: net.clone(), addr: addr(27015) }, Identity::generate());
        let alice = Identity::generate();
        // the server's key known beforehand: as when it was played on before
        let client = Client::with_wire(
            TestWire { net: net.clone(), addr: addr(50000) },
            addr(27015),
            b"hello, I am Alice",
            &alice,
            Some(server.key()),
            start,
        );
        let mut pair = Pair { net, server, client, alice, id: 0, now: start, start, server_got: Vec::new(), client_got: Vec::new() };
        let mut accepted = false;
        for _ in 0..1000 {
            pair.server.flush(pair.now);
            pair.client.flush(pair.now);
            pair.now += Duration::from_millis(10);
            pair.net.borrow_mut().step += 1;
            for event in pair.server.poll(pair.now) {
                if let ServerEvent::Request { addr: from, hello, key } = event {
                    assert_eq!(hello, b"hello, I am Alice");
                    assert_eq!(key, pair.alice.public(), "who asks is who they say");
                    pair.id = pair.server.accept(from, b"welcome to gm_construct", pair.now).unwrap();
                }
            }
            for event in pair.client.poll(pair.now) {
                if let ClientEvent::Accepted { welcome, key } = event {
                    assert_eq!(welcome, b"welcome to gm_construct");
                    assert_eq!(key, pair.server.key());
                    accepted = true;
                }
            }
            if accepted {
                break;
            }
        }
        assert!(accepted, "connected");
        pair
    }

    /// 10 ms: both sides send what is due and read what came.
    fn step(&mut self) {
        self.server.flush(self.now);
        self.client.flush(self.now);
        self.now += Duration::from_millis(10);
        self.net.borrow_mut().step += 1;
        for event in self.server.poll(self.now) {
            match event {
                ServerEvent::Message { client, data } => {
                    assert_eq!(client, self.id);
                    self.server_got.push(data);
                }
                other => panic!("{other:?}"),
            }
        }
        for event in self.client.poll(self.now) {
            match event {
                ClientEvent::Message { data } => self.client_got.push(data),
                other => panic!("{other:?}"),
            }
        }
    }
}

#[test]
fn a_client_connects_and_both_talk() {
    let mut p = Pair::new(0, 0, 0);
    p.client.send(b"say hi", true).unwrap();
    p.client.send(b"keys", false).unwrap();
    p.server.send(p.id, b"snapshot", false).unwrap();
    p.server.send(p.id, b"", true).unwrap();
    for _ in 0..5 {
        p.step();
    }
    // each channel keeps its own order; between them there is none
    p.server_got.sort();
    p.client_got.sort();
    assert_eq!(p.server_got, [b"keys".to_vec(), b"say hi".to_vec()]);
    assert_eq!(p.client_got, [Vec::new(), b"snapshot".to_vec()]);
    assert!(p.client.is_connected() && p.server.clients() == [p.id]);
}

#[test]
fn reliable_messages_arrive_whole_once_and_in_order_through_a_bad_network() {
    // a third lost, some twice, some overtaking others by up to 80 ms
    let mut p = Pair::new(30, 10, 8);
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 + i / 1000) as u8).collect();
    let mut sent = Vec::new();
    for i in 0..200u32 {
        let message = if i == 100 { big.clone() } else { format!("message {i}").into_bytes() };
        p.server.send(p.id, &message, true).unwrap();
        p.client.send(&message, true).unwrap();
        sent.push(message);
        p.step();
    }
    for _ in 0..3000 {
        if p.client_got.len() == sent.len() && p.server_got.len() == sent.len() {
            break;
        }
        p.step();
    }
    assert_eq!(p.client_got.len(), sent.len());
    assert!(p.client_got == sent, "the client got them in order");
    assert!(p.server_got == sent, "the server got them in order");
    // the last acknowledgements on their way
    for _ in 0..100 {
        p.step();
    }
    assert_eq!((p.server.pending(p.id), p.client.pending()), (0, 0));
    // three megabytes at two a second, a third of it again
    let elapsed = p.now.duration_since(p.start) - Duration::from_secs(1);
    assert!(elapsed > Duration::from_secs(1) && elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn the_rate_holds_big_messages_back() {
    let mut p = Pair::new(0, 0, 0);
    p.server.set_rate(100_000);
    p.server.send(p.id, &vec![7; 300_000], true).unwrap();
    p.server.send(p.id, b"after", true).unwrap();
    let mut steps = 0;
    while p.client_got.len() < 2 {
        p.step();
        steps += 1;
        assert!(steps < 1000);
    }
    // 300 kB at 100 kB a second: about three seconds of 10 ms steps
    assert!((280..340).contains(&steps), "{steps}");
    assert_eq!(p.client_got[1], b"after");
}

#[test]
fn unreliable_messages_come_whole_or_not_at_all() {
    let mut p = Pair::new(0, 0, 0);
    let big: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
    p.server.send(p.id, &big, false).unwrap();
    for _ in 0..3 {
        p.step();
    }
    assert_eq!(p.client_got, std::slice::from_ref(&big), "in fragments, whole");
    assert_eq!(p.server.send(p.id, &vec![0; MAX_UNRELIABLE + 1], false), Err(TooBig));
    // through loss: each that comes is whole
    let mut p = Pair::new(20, 0, 3);
    for i in 0..50u8 {
        p.server.send(p.id, &vec![i; 5000], false).unwrap();
        p.step();
    }
    for _ in 0..50 {
        p.step();
    }
    assert!(!p.client_got.is_empty() && p.client_got.len() < 50, "{}", p.client_got.len());
    assert!(p.client_got.iter().all(|m| m.len() == 5000 && m.iter().all(|&b| b == m[0])));
}

#[test]
fn turned_away_timed_out_and_gone() {
    let net = Rc::new(RefCell::new(Net { dice: 7, ..Net::default() }));
    let mut now = Instant::now();
    let mut server = Server::with_wire(TestWire { net: net.clone(), addr: addr(27015) }, Identity::generate());
    let bob = Identity::generate();
    let mut client = Client::with_wire(TestWire { net: net.clone(), addr: addr(50000) }, addr(27015), b"bob", &bob, None, now);
    let mut denied = None;
    for _ in 0..50 {
        client.flush(now);
        net.borrow_mut().step += 1;
        now += Duration::from_millis(10);
        for event in server.poll(now) {
            if let ServerEvent::Request { addr, .. } = event {
                server.deny(addr, "the server is full");
            }
        }
        for event in client.poll(now) {
            if let ClientEvent::Denied { reason } = event {
                denied = Some(reason);
            }
        }
    }
    assert_eq!(denied.as_deref(), Some("the server is full"));
    // a server that never answers
    let mut lonely = Client::with_wire(TestWire { net: net.clone(), addr: addr(50001) }, addr(9), b"x", &bob, None, now);
    let mut events = Vec::new();
    for _ in 0..1200 {
        lonely.flush(now);
        now += Duration::from_millis(10);
        events.extend(lonely.poll(now));
    }
    assert!(matches!(events.as_slice(), [ClientEvent::Denied { .. }]), "{events:?}");

    // connected, then the client goes quiet: the server lets it go after 15 s
    let mut p = Pair::new(0, 0, 0);
    let mut gone = Vec::new();
    for _ in 0..1700 {
        p.server.flush(p.now);
        p.now += Duration::from_millis(10);
        p.net.borrow_mut().step += 1;
        gone.extend(p.server.poll(p.now));
    }
    assert!(matches!(gone.as_slice(), [ServerEvent::Disconnected { reason, .. }] if reason == "timed out"), "{gone:?}");

    // leaving says so
    let mut p = Pair::new(0, 0, 0);
    p.client.disconnect("bye");
    p.net.borrow_mut().step += 1;
    let events = p.server.poll(p.now);
    assert!(matches!(events.as_slice(), [ServerEvent::Disconnected { reason, .. }] if reason == "bye"), "{events:?}");
    assert!(p.server.clients().is_empty());
}

#[test]
fn junk_does_no_harm() {
    let mut p = Pair::new(0, 0, 0);
    let mut dice = 99u64;
    for len in 0..400usize {
        let mut junk = Vec::with_capacity(len);
        for _ in 0..len {
            dice ^= dice << 13;
            dice ^= dice >> 7;
            dice ^= dice << 17;
            junk.push(dice as u8);
        }
        // sometimes looking like ours
        if len % 3 == 0 && len >= 5 {
            junk[..4].copy_from_slice(&MAGIC);
            junk[4] = (len % 6) as u8;
        }
        // even knowing the connection, from where its sides are: what does not open is not
        // there
        if len % 5 == 0 && len >= 13 {
            junk[5..13].copy_from_slice(&p.id.to_le_bytes());
            junk[4] = if len % 2 == 0 { DATA } else { DISCONNECT };
        }
        p.net.borrow_mut().flying.push((0, addr(27015), addr(50000), junk.clone()));
        p.net.borrow_mut().flying.push((0, addr(50000), addr(27015), junk));
        p.server.poll(p.now);
        p.client.poll(p.now);
    }
    // still talking
    p.client.send(b"still here", true).unwrap();
    for _ in 0..30 {
        p.step();
    }
    assert!(p.server_got.contains(&b"still here".to_vec()));
}

#[test]
fn over_real_sockets() {
    let start = Instant::now();
    let mut server = Server::bind("127.0.0.1:0".parse().unwrap(), Identity::generate()).unwrap();
    let at = server.local_addr().unwrap();
    let mut client = Client::connect(at, b"udp", &Identity::generate(), None, start).unwrap();
    let mut got = Vec::new();
    let mut id = None;
    for i in 0..400 {
        let now = start + Duration::from_millis(5 * i);
        client.flush(now);
        for event in server.poll(now) {
            match event {
                ServerEvent::Request { addr, .. } => {
                    let client = server.accept(addr, b"ok", now).unwrap();
                    server.send(client, b"to the client", true).unwrap();
                    id = Some(client);
                }
                ServerEvent::Message { data, .. } => got.push(data),
                ServerEvent::Disconnected { .. } => {}
            }
        }
        server.flush(now);
        for event in client.poll(now) {
            if let ClientEvent::Message { data } = event {
                got.push(data);
                client.send(b"to the server", true).unwrap();
            }
        }
        if got.len() == 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(id.is_some());
    assert_eq!(got, [b"to the client".to_vec(), b"to the server".to_vec()]);
}

#[test]
fn servers_say_what_they_are_to_whoever_asks() {
    let start = Instant::now();
    let mut server = Server::bind("127.0.0.1:0".parse().unwrap(), Identity::generate()).unwrap();
    let at = server.local_addr().unwrap();
    let mut query = Query::with_wire(Udp::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    // with nothing to say it says nothing
    query.ask(at, start).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    server.poll(start);
    std::thread::sleep(Duration::from_millis(20));
    assert!(query.poll(start).is_empty());
    let infos = |replies: Vec<Reply>| -> Vec<Answer> {
        replies.into_iter().filter_map(|r| if let Reply::Info(a) = r { Some(a) } else { None }).collect()
    };
    // asked, it answers the question with what it is
    server.set_info(b"gm_construct, 3 of 8");
    let question = query.ask(at, start).unwrap();
    let mut answers = Vec::new();
    for _ in 0..200 {
        server.poll(start);
        answers.extend(infos(query.poll(start + Duration::from_millis(30))));
        if !answers.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(answers.len(), 1);
    let answer = &answers[0];
    assert_eq!((answer.question, answer.asked, answer.from), (question, at, at));
    assert_eq!((answer.info.as_slice(), answer.took), (&b"gm_construct, 3 of 8"[..], Duration::from_millis(30)));
    // a short question - which would make it an amplifier - gets nothing, nor does too long an answer go out
    let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    raw.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
    let mut short = b"OGMN".to_vec();
    short.push(6);
    short.extend(7u64.to_le_bytes());
    raw.send_to(&short, at).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    server.poll(start);
    let mut buf = [0u8; 2048];
    assert!(raw.recv_from(&mut buf).is_err());
    server.set_info(&vec![1; MAX_INFO + 1]);
    query.ask(at, start).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    server.poll(start);
    std::thread::sleep(Duration::from_millis(20));
    assert!(query.poll(start).is_empty());
}

/// Runs a server and a client on a network for `steps` of 10 ms, the server accepting whoever
/// asks; what each heard.
fn exchange(
    net: &Rc<RefCell<Net>>,
    server: &mut Server<TestWire>,
    client: &mut Client<TestWire>,
    steps: usize,
) -> (Vec<ServerEvent>, Vec<ClientEvent>) {
    let (mut heard, mut told) = (Vec::new(), Vec::new());
    let mut now = Instant::now();
    for _ in 0..steps {
        server.flush(now);
        client.flush(now);
        now += Duration::from_millis(10);
        net.borrow_mut().step += 1;
        for event in server.poll(now) {
            if let ServerEvent::Request { addr, .. } = &event {
                server.accept(*addr, b"welcome", now);
            }
            heard.push(event);
        }
        told.extend(client.poll(now));
    }
    (heard, told)
}

#[test]
fn a_server_is_known_by_its_key_and_a_player_by_theirs() {
    let net = Rc::new(RefCell::new(Net { dice: 3, ..Net::default() }));
    let wire = |port| TestWire { net: net.clone(), addr: addr(port) };
    let (real, impostor, alice) = (Identity::generate(), Identity::generate(), Identity::generate());
    // a server that is not the one the client played on before - with its address, say - is
    // not answered: it never hears who the player is
    let mut server = Server::with_wire(wire(27015), impostor.clone());
    let mut client = Client::with_wire(wire(50000), addr(27015), b"alice", &alice, Some(real.public()), Instant::now());
    let (heard, told) = exchange(&net, &mut server, &mut client, 50);
    assert!(heard.is_empty(), "{heard:?}");
    assert_eq!(told, [ClientEvent::KeyChanged { key: impostor.public() }]);
    // the one it knows - or one it does not know yet - it joins, and each learns the other's key
    let mut server = Server::with_wire(wire(27016), real.clone());
    for expect in [Some(real.public()), None] {
        let mut client = Client::with_wire(wire(50001), addr(27016), b"alice", &alice, expect, Instant::now());
        let (heard, told) = exchange(&net, &mut server, &mut client, 50);
        assert!(heard.iter().any(|e| matches!(e, ServerEvent::Request { key, .. } if *key == alice.public())), "{heard:?}");
        assert!(told.iter().any(|e| matches!(e, ClientEvent::Accepted { key, .. } if *key == real.public())), "{told:?}");
    }

    // someone who has the server's challenge - from the path - cannot answer it as another
    // player: the answer is signed by who it names
    let mut server = Server::with_wire(wire(27017), real);
    let mallory = Identity::generate();
    let salt = 77u64;
    let mut ask = control(CONNECT, &[&PROTOCOL.to_le_bytes(), &salt.to_le_bytes()]);
    ask.resize(MAX_PACKET, 0);
    net.borrow_mut().flying.push((0, addr(27017), addr(50002), ask));
    server.poll(Instant::now());
    let challenge = {
        let mut net = net.borrow_mut();
        let i = net.flying.iter().position(|(_, to, from, _)| *to == addr(50002) && *from == addr(27017)).expect("challenged");
        net.flying.remove(i).3
    };
    let theirs: [u8; KEY] = challenge[13..13 + KEY].try_into().unwrap();
    for (named, signer) in [(alice.public(), &mallory), (mallory.public(), &mallory)] {
        let ours = Ephemeral::generate();
        let mine = ours.public;
        let said = transcript(salt, &server.key(), &theirs, &named, &mine);
        let keys = ours.agree(&theirs, &said).unwrap();
        let mut join = control(JOIN, &[&salt.to_le_bytes(), &mine, &named.0, &signer.sign(&said), b"hi"]);
        Sealer::seal_numbered(&keys.client, 0, &mut join, JOIN_HEADER);
        net.borrow_mut().flying.push((0, addr(27017), addr(50002), join));
        let events = server.poll(Instant::now());
        if named == mallory.public() {
            assert!(matches!(events.as_slice(), [ServerEvent::Request { key, .. }] if *key == mallory.public()), "{events:?}");
        } else {
            assert!(events.is_empty(), "passed for Alice: {events:?}");
        }
    }
}

#[test]
fn what_goes_between_them_is_sealed() {
    let mut p = Pair::new(0, 0, 0);
    let secret = b"the password is swordfish";
    p.client.send(secret, true).unwrap();
    p.client.flush(p.now);
    let recorded: Vec<Vec<u8>> = p.net.borrow().flying.iter().filter(|f| f.1 == addr(27015)).map(|f| f.3.clone()).collect();
    assert!(!recorded.is_empty());
    assert!(recorded.iter().all(|packet| !packet.windows(secret.len()).any(|w| w == secret)), "readable on the way");
    for _ in 0..5 {
        p.step();
    }
    assert_eq!(p.server_got, [secret.to_vec()]);

    // recorded and sent again - even once the sequence numbers have gone round - it does not
    // come again
    p.server.peers[0].newest = None;
    p.server.peers[0].seen = vec![None; SEEN];
    p.server.peers[0].expect = 0;
    for packet in &recorded {
        p.net.borrow_mut().flying.push((0, addr(27015), addr(50000), packet.clone()));
    }
    for _ in 0..5 {
        p.step();
    }
    assert_eq!(p.server_got.len(), 1, "came again");

    // changed on the way, a packet is not there; the connection goes on
    p.server.send(p.id, b"to Alice", true).unwrap();
    p.server.flush(p.now);
    for flying in p.net.borrow_mut().flying.iter_mut().filter(|f| f.1 == addr(50000)) {
        let last = flying.3.len() - 1;
        flying.3[last] ^= 0x40;
    }
    p.net.borrow_mut().step += 1;
    p.client.poll(p.now);
    assert!(p.client_got.is_empty());
    for _ in 0..30 {
        p.step();
    }
    assert_eq!(p.client_got, [b"to Alice".to_vec()], "sent again, whole");
}

#[test]
fn old_and_short_requests_get_no_more_than_they_sent() {
    let net = Rc::new(RefCell::new(Net { dice: 5, ..Net::default() }));
    let mut server = Server::with_wire(TestWire { net: net.clone(), addr: addr(27015) }, Identity::generate());
    let answers = |request: Vec<u8>, server: &mut Server<TestWire>| {
        net.borrow_mut().flying.push((0, addr(27015), addr(50000), request));
        server.poll(Instant::now());
        std::mem::take(&mut net.borrow_mut().flying).into_iter().map(|f| f.3).collect::<Vec<_>>()
    };
    // a client of the first protocol: told so, in no more bytes than it sent
    let old = control(CONNECT, &[&1u16.to_le_bytes(), &5u64.to_le_bytes(), b"hello"]);
    let told = answers(old.clone(), &mut server);
    assert_eq!(told.len(), 1);
    assert!(told[0].len() <= old.len() && told[0][4] == DENY, "{told:?}");
    // this protocol's, unpadded: nothing
    let short = control(CONNECT, &[&PROTOCOL.to_le_bytes(), &6u64.to_le_bytes()]);
    assert!(answers(short, &mut server).is_empty());
    let mut padded = control(CONNECT, &[&PROTOCOL.to_le_bytes(), &6u64.to_le_bytes()]);
    padded.resize(MAX_PACKET, 0);
    let told = answers(padded, &mut server);
    assert!(told.len() == 1 && told[0][4] == CHALLENGE && told[0].len() < MAX_PACKET, "{told:?}");
}

#[test]
fn a_master_lists_the_servers_that_are_there() {
    let net = Rc::new(RefCell::new(Net { dice: 11, ..Net::default() }));
    let wire = |port| TestWire { net: net.clone(), addr: addr(port) };
    let mut master = Master::with_wire(wire(27011));
    let mut server = Server::with_wire(wire(27015), Identity::generate());
    let mut quiet = Server::with_wire(wire(27016), Identity::generate());
    let mut query = Query::with_wire(wire(50000));
    let mut now = Instant::now();
    let run = |master: &mut Master<TestWire>, servers: &mut [&mut Server<TestWire>], now: Instant| {
        for _ in 0..4 {
            net.borrow_mut().step += 1;
            master.poll(now);
            for server in servers.iter_mut() {
                server.poll(now);
            }
        }
    };
    // a server that says what it is is listed; one with nothing to say, or an address that
    // only claims to be a server, is not
    server.set_info(b"gm_construct");
    server.announce(addr(27011)).unwrap();
    quiet.announce(addr(27011)).unwrap();
    let mut forged = control(HEARTBEAT, &[&PROTOCOL.to_le_bytes()]);
    forged.resize(MAX_PACKET, 0);
    net.borrow_mut().flying.push((0, addr(27011), addr(40000), forged.clone()));
    // nor does a short one get an answer
    net.borrow_mut().flying.push((0, addr(27011), addr(40001), forged[..20].to_vec()));
    run(&mut master, &mut [&mut server, &mut quiet], now);
    assert_eq!(master.servers(), [addr(27015)]);
    assert!(net.borrow().flying.iter().all(|f| f.1 != addr(40001)), "answered a short heartbeat");

    // a browser asks for the list, a page at a time, of its own protocol
    let asked = query.ask_master(addr(27011), 0, now).unwrap();
    run(&mut master, &mut [], now);
    let replies = query.poll(now);
    assert!(
        matches!(replies.as_slice(), [Reply::Page(Page { question, total: 1, start: 0, servers, .. })] if *question == asked && servers == &[addr(27015)]),
        "{replies:?}"
    );
    let mut short = control(LIST, &[&7u64.to_le_bytes(), &PROTOCOL.to_le_bytes(), &0u32.to_le_bytes()]);
    net.borrow_mut().flying.push((0, addr(27011), addr(40002), std::mem::take(&mut short)));
    run(&mut master, &mut [], now);
    assert!(net.borrow().flying.iter().all(|f| f.1 != addr(40002)), "answered a short question");

    // many servers come in pages
    let mut many: Vec<Server<TestWire>> = (0..200).map(|i| Server::with_wire(wire(30000 + i), Identity::generate())).collect();
    for s in &mut many {
        s.set_info(b"x");
        s.announce(addr(27011)).unwrap();
    }
    let mut all: Vec<&mut Server<TestWire>> = many.iter_mut().collect();
    run(&mut master, &mut all, now);
    let mut listed = Vec::new();
    let mut total = u32::MAX;
    while (listed.len() as u32) < total {
        query.ask_master(addr(27011), listed.len() as u32, now).unwrap();
        run(&mut master, &mut [], now);
        let Some(Reply::Page(page)) = query.poll(now).pop() else { panic!("no page") };
        assert!(!page.servers.is_empty() && page.servers.len() < 200);
        total = page.total;
        listed.extend(page.servers);
    }
    assert_eq!(listed.len(), 201);

    // one that stops announcing itself goes after a while
    now += Duration::from_secs(200);
    server.announce(addr(27011)).unwrap();
    run(&mut master, &mut [&mut server], now);
    now += Duration::from_secs(200);
    run(&mut master, &mut [], now);
    assert_eq!(master.servers(), [addr(27015)]);
}
