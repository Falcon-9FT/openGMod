use std::cell::RefCell;
use std::rc::Rc;

use super::*;

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
        let server = Server::with_wire(TestWire { net: net.clone(), addr: addr(27015) });
        let client = Client::with_wire(TestWire { net: net.clone(), addr: addr(50000) }, addr(27015), b"hello, I am Alice", start);
        let mut pair = Pair { net, server, client, id: 0, now: start, start, server_got: Vec::new(), client_got: Vec::new() };
        let mut accepted = false;
        for _ in 0..1000 {
            pair.server.flush(pair.now);
            pair.client.flush(pair.now);
            pair.now += Duration::from_millis(10);
            pair.net.borrow_mut().step += 1;
            for event in pair.server.poll(pair.now) {
                if let ServerEvent::Request { addr: from, hello } = event {
                    assert_eq!(hello, b"hello, I am Alice");
                    pair.id = pair.server.accept(from, b"welcome to gm_construct", pair.now).unwrap();
                }
            }
            for event in pair.client.poll(pair.now) {
                if let ClientEvent::Accepted { welcome } = event {
                    assert_eq!(welcome, b"welcome to gm_construct");
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
    let mut server = Server::with_wire(TestWire { net: net.clone(), addr: addr(27015) });
    let mut client = Client::with_wire(TestWire { net: net.clone(), addr: addr(50000) }, addr(27015), b"bob", now);
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
    let mut lonely = Client::with_wire(TestWire { net: net.clone(), addr: addr(50001) }, addr(9), b"x", now);
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
        // even knowing the connection, from somewhere else (only someone on the path who
        // learned the connection's number could pass for the other side, and they could as
        // well drop everything)
        let stranger = len % 5 == 0 && len >= 13;
        if stranger {
            junk[5..13].copy_from_slice(&p.id.to_le_bytes());
            junk[4] = DATA;
        }
        let (to_client, to_server) = if stranger { (addr(40000), addr(40000)) } else { (addr(27015), addr(50000)) };
        p.net.borrow_mut().flying.push((0, addr(27015), to_server, junk.clone()));
        p.net.borrow_mut().flying.push((0, addr(50000), to_client, junk));
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
    let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let at = server.local_addr().unwrap();
    let mut client = Client::connect(at, b"udp", start).unwrap();
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
