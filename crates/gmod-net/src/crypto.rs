//! What keeps a connection private and its two sides who they say they are.
//!
//! Each side has an identity: an Ed25519 key pair it keeps (a player's in their openGMOD
//! folder, a server's in its own), its public half what others know it by. Each connection
//! has keys of its own: both sides make an X25519 key pair for it alone, and what the two
//! agree on (with HKDF-SHA256, over everything the handshake said) gives a ChaCha20-Poly1305
//! key for each way. Each side signs the other's key pair for the connection with its
//! identity, so no one between them can stand in for either; and since the keys of a
//! connection are forgotten with it, what was recorded of it can never be read later.
//!
//! A packet sealed carries its number in the clear: the nonce, never used twice with a key,
//! and what tells a packet sent again by someone who recorded it ([`Replay`]).

use std::io;
use std::path::Path;

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use ring::{agreement, hkdf};

/// How long a key, public or of a connection, is.
pub const KEY: usize = 32;
/// How long a signature is.
pub const SIGNATURE: usize = 64;
/// What sealing adds to a packet: the tag that proves it whole.
pub const TAG: usize = 16;
/// How many packets back one still counts as new, if it has not come yet.
const REPLAY: u64 = 1024;

/// A side's public key: what it is known by.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey(pub [u8; KEY]);

impl PublicKey {
    /// The key written out: 64 hexadecimal digits.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(text: &str) -> Option<PublicKey> {
        Some(PublicKey(hex(text.trim())?))
    }

    /// Whether `signature` is this key's, of `message`.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ED25519, self.0).verify(message, signature).is_ok()
    }

    /// The key hashed (SHA-256): what is made of it is the same for the same key only.
    pub fn digest(&self) -> [u8; 32] {
        let digest = ring::digest::digest(&ring::digest::SHA256, &self.0);
        let mut out = [0; 32];
        out.copy_from_slice(digest.as_ref());
        out
    }
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// `N` bytes from hexadecimal digits.
fn hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Bytes no one can guess beforehand, from the system.
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0; N];
    // the system's generator fails only where nothing would run anyway
    SystemRandom::new().fill(&mut out).expect("the system gives no random numbers");
    out
}

/// A side's identity: its key pair. Only its seed is kept; whoever has that is this side.
#[derive(Clone)]
pub struct Identity {
    seed: [u8; KEY],
    public: PublicKey,
}

impl std::fmt::Debug for Identity {
    // the seed is secret: never in a log
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Identity({})", self.public.to_hex())
    }
}

impl Identity {
    /// A new identity.
    pub fn generate() -> Identity {
        Identity::from_seed(random_bytes()).expect("any 32 bytes make an Ed25519 key")
    }

    /// The identity a seed makes.
    pub fn from_seed(seed: [u8; KEY]) -> Option<Identity> {
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).ok()?;
        let public = PublicKey(pair.public_key().as_ref().try_into().ok()?);
        Some(Identity { seed, public })
    }

    pub fn public(&self) -> PublicKey {
        self.public
    }

    /// Signs `message`.
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE] {
        let pair = Ed25519KeyPair::from_seed_unchecked(&self.seed).expect("made from this seed before");
        let mut out = [0; SIGNATURE];
        out.copy_from_slice(pair.sign(message).as_ref());
        out
    }

    /// The identity kept in the file at `path` - the seed in hexadecimal, a line - made and
    /// written there if there is none (readable by its owner only). A file that is there but
    /// says no seed is not replaced: it is an error, as is one that cannot be written.
    pub fn load_or_create(path: &Path) -> io::Result<Identity> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let seed = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).and_then(hex::<KEY>);
                seed.and_then(Identity::from_seed).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("{} holds no key; remove it for a new one", path.display()))
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = Identity::generate();
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let seed: String = identity.seed.iter().map(|b| format!("{b:02x}")).collect();
                let text = format!(
                    "# openGMOD's secret key: whoever has this file is you to the servers (or is this server to its players).\n\
                     # Never give it away. The public key: {}\n{seed}\n",
                    identity.public.to_hex()
                );
                write_private(path, text.as_bytes())?;
                Ok(identity)
            }
            Err(e) => Err(e),
        }
    }
}

/// Writes a file that only its owner may read.
fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

/// A key pair for one connection, and its public half.
pub(crate) struct Ephemeral {
    private: agreement::EphemeralPrivateKey,
    pub public: [u8; KEY],
}

impl Ephemeral {
    pub fn generate() -> Ephemeral {
        let private =
            agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new()).expect("the system gives no random numbers");
        let public = private.compute_public_key().ok().and_then(|k| k.as_ref().try_into().ok()).expect("an X25519 key is 32 bytes");
        Ephemeral { private, public }
    }

    /// What this key and the other side's agree on, made into a key for each way: the
    /// client's (to the server) and the server's. `transcript` is everything the handshake
    /// said, so a key is this conversation's only. `None` for a public key that is none.
    pub fn agree(self, other: &[u8; KEY], transcript: &[u8]) -> Option<SessionKeys> {
        agreement::agree_ephemeral(self.private, &agreement::UnparsedPublicKey::new(&agreement::X25519, other), |secret| {
            let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, b"openGMOD connection keys").extract(secret);
            let key = |way: &[u8]| -> Option<[u8; KEY]> {
                let mut out = [0; KEY];
                prk.expand(&[way, transcript], Bytes(KEY)).ok()?.fill(&mut out).ok()?;
                Some(out)
            };
            Some(SessionKeys { client: key(b"client to server")?, server: key(b"server to client")? })
        })
        .ok()
        .flatten()
    }
}

/// A length for HKDF to fill.
struct Bytes(usize);

impl hkdf::KeyType for Bytes {
    fn len(&self) -> usize {
        self.0
    }
}

/// A connection's keys, a way each.
pub(crate) struct SessionKeys {
    /// What the client seals and the server opens.
    pub client: [u8; KEY],
    /// What the server seals and the client opens.
    pub server: [u8; KEY],
}

fn aead(key: &[u8; KEY]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("a ChaCha20 key is 32 bytes"))
}

fn nonce(number: u64) -> Nonce {
    let mut n = [0; 12];
    n[4..].copy_from_slice(&number.to_le_bytes());
    Nonce::assume_unique_for_key(n)
}

/// Seals packets one way: each with the next number.
pub(crate) struct Sealer {
    key: LessSafeKey,
    next: u64,
}

impl Sealer {
    /// A sealer whose first packet is numbered `first`: what came before it on this key was
    /// the handshake's.
    pub fn new(key: &[u8; KEY], first: u64) -> Sealer {
        Sealer { key: aead(key), next: first }
    }

    /// Seals what follows the first `clear` bytes of `packet`, which stay readable but are
    /// proven whole too; `number` is the packet's.
    pub fn seal_numbered(key: &[u8; KEY], number: u64, packet: &mut Vec<u8>, clear: usize) {
        let (head, body) = packet.split_at_mut(clear);
        let tag = aead(key).seal_in_place_separate_tag(nonce(number), Aad::from(&head[..]), body).expect("a packet is never that long");
        packet.extend_from_slice(tag.as_ref());
    }

    /// Seals `packet`, whose bytes `at..at + 8` are for its number and are put there; the
    /// bytes before stay readable.
    pub fn seal(&mut self, packet: &mut Vec<u8>, at: usize) {
        let number = self.next;
        self.next += 1;
        packet[at..at + 8].copy_from_slice(&number.to_le_bytes());
        let (head, body) = packet.split_at_mut(at + 8);
        let tag = self.key.seal_in_place_separate_tag(nonce(number), Aad::from(&head[..]), body).expect("a packet is never that long");
        packet.extend_from_slice(tag.as_ref());
    }
}

/// Opens packets one way, each once.
pub(crate) struct Opener {
    key: LessSafeKey,
    replay: Replay,
}

impl Opener {
    /// An opener for packets numbered `first` on.
    pub fn new(key: &[u8; KEY], first: u64) -> Opener {
        Opener { key: aead(key), replay: Replay::from(first) }
    }

    /// Opens a packet numbered with what `numbered` sealed; `None` when it is not whole, not
    /// this key's, or not numbered `number`.
    pub fn open_numbered<'a>(key: &[u8; KEY], number: u64, packet: &'a mut [u8], clear: usize) -> Option<&'a [u8]> {
        if packet.len() < clear + TAG {
            return None;
        }
        let (head, body) = packet.split_at_mut(clear);
        aead(key).open_in_place(nonce(number), Aad::from(&head[..]), body).ok().map(|b| &*b)
    }

    /// Opens a packet whose number is at `at..at + 8`, the bytes before it readable: what is
    /// in it, or `None` when it is not whole, not this key's, or came before.
    pub fn open<'a>(&mut self, packet: &'a mut [u8], at: usize) -> Option<&'a [u8]> {
        if packet.len() < at + 8 + TAG {
            return None;
        }
        let number = u64::from_le_bytes(packet[at..at + 8].try_into().ok()?);
        if !self.replay.fresh(number) {
            return None;
        }
        let (head, body) = packet.split_at_mut(at + 8);
        let opened = self.key.open_in_place(nonce(number), Aad::from(&head[..]), body).ok()?;
        self.replay.mark(number);
        Some(opened)
    }
}

/// Which packet numbers came, of the last [`REPLAY`]: a packet recorded and sent again is
/// dropped, as is one so old it cannot be told.
pub(crate) struct Replay {
    /// One past the highest number that came.
    next: u64,
    /// A bit for each of the last numbers, by the number's place in a ring.
    seen: [u64; (REPLAY / 64) as usize],
}

impl Replay {
    /// A window where the numbers before `first` have come.
    pub fn from(first: u64) -> Replay {
        let mut replay = Replay { next: 0, seen: [0; (REPLAY / 64) as usize] };
        for number in first.saturating_sub(REPLAY)..first {
            replay.mark(number);
        }
        replay.next = first;
        replay
    }

    fn bit(number: u64) -> (usize, u64) {
        let i = number % REPLAY;
        ((i / 64) as usize, 1 << (i % 64))
    }

    /// Whether `number` has not come, and is recent enough to tell.
    pub fn fresh(&self, number: u64) -> bool {
        if number >= self.next {
            return number != u64::MAX;
        }
        if self.next - number > REPLAY {
            return false;
        }
        let (word, bit) = Replay::bit(number);
        self.seen[word] & bit == 0
    }

    /// `number` came.
    pub fn mark(&mut self, number: u64) {
        if number >= self.next {
            // the numbers skipped take their places in the ring from those now too old
            if number - self.next >= REPLAY {
                self.seen = [0; (REPLAY / 64) as usize];
            } else {
                for skipped in self.next..number {
                    let (word, bit) = Replay::bit(skipped);
                    self.seen[word] &= !bit;
                }
            }
            self.next = number + 1;
        }
        let (word, bit) = Replay::bit(number);
        self.seen[word] |= bit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_open_once_and_only_whole() {
        let keys = [7u8; KEY];
        let mut sealer = Sealer::new(&keys, 1);
        let mut opener = Opener::new(&keys, 1);
        let packets: Vec<Vec<u8>> = (0..5)
            .map(|i| {
                let mut p = b"OGMN\x04 conn".to_vec();
                p.extend_from_slice(&[0; 8]);
                p.extend_from_slice(format!("message {i}").as_bytes());
                sealer.seal(&mut p, 10);
                p
            })
            .collect();
        // out of order is fine, twice is not
        for i in [1, 0, 3] {
            let mut p = packets[i].clone();
            assert_eq!(opener.open(&mut p, 10), Some(format!("message {i}").as_bytes()));
        }
        assert_eq!(opener.open(&mut packets[1].clone(), 10), None, "sent again");
        // a bit changed anywhere, in the clear or not, and it is no packet
        for at in [0, 5, 12, packets[4].len() - 1] {
            let mut p = packets[4].clone();
            p[at] ^= 1;
            assert_eq!(opener.open(&mut p, 10), None, "changed at {at}");
        }
        assert!(opener.open(&mut packets[4].clone(), 10).is_some());
        // another key's
        let mut other = Opener::new(&[8; KEY], 1);
        assert_eq!(other.open(&mut packets[2].clone(), 10), None);
    }

    #[test]
    fn the_replay_window_forgets_only_what_is_too_old() {
        let mut replay = Replay::from(1);
        assert!(!replay.fresh(0), "before the first");
        replay.mark(5);
        assert!(replay.fresh(1) && replay.fresh(4) && !replay.fresh(5));
        replay.mark(5 + REPLAY);
        assert!(!replay.fresh(4), "too old");
        assert!(replay.fresh(6), "still within");
        replay.mark(6);
        assert!(!replay.fresh(6));
        replay.mark(10 * REPLAY);
        assert!(!replay.fresh(5 + REPLAY), "too old now");
        assert!(replay.fresh(10 * REPLAY - 1));
    }

    #[test]
    fn identities_sign_and_are_kept() {
        let dir = std::env::temp_dir().join(format!("opengmod-identity-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("keys").join("identity.key");
        let made = Identity::load_or_create(&path).unwrap();
        let again = Identity::load_or_create(&path).unwrap();
        assert_eq!(made.public(), again.public());
        let signature = again.sign(b"hello");
        assert!(made.public().verify(b"hello", &signature));
        assert!(!made.public().verify(b"hullo", &signature));
        assert!(!Identity::generate().public().verify(b"hello", &signature));
        assert_eq!(PublicKey::from_hex(&made.public().to_hex()), Some(made.public()));
        assert!(!format!("{made:?}").contains(&std::fs::read_to_string(&path).unwrap().lines().last().unwrap()[..16]));
        std::fs::write(&path, "nothing").unwrap();
        assert!(Identity::load_or_create(&path).is_err(), "a spoiled file is not replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_sides_agree_on_the_keys() {
        let (a, b) = (Ephemeral::generate(), Ephemeral::generate());
        let (a_public, b_public) = (a.public, b.public);
        let one = a.agree(&b_public, b"transcript").unwrap();
        let other = b.agree(&a_public, b"transcript").unwrap();
        assert_eq!((one.client, one.server), (other.client, other.server));
        assert_ne!(one.client, one.server);
        let third = Ephemeral::generate().agree(&a_public, b"transcript").unwrap();
        assert_ne!(third.client, one.client);
        assert!(Ephemeral::generate().agree(&[0; KEY], b"transcript").is_none(), "no key at all");
    }
}
