//! Who the player and the server are on the network: their keys ([`gmod_net::Identity`]), kept
//! in openGMOD's folder - the player's in `identity.key`, a server's in `server.key` - and the
//! keys of the servers the player has met (`servers/known.txt`), as SSH keeps them: a server
//! known by another key than the one it had is not joined, for it may be another one standing
//! in for it.
//!
//! No Steam vouches for anyone, so a player's `SteamID64` is openGMOD's own, made from their
//! key: the same on every server for whoever has the key, and no Steam account's. Addons that
//! keep what a player has by it (bans, ranks, saves) work as they would; it only means
//! nothing to Steam.

use std::path::PathBuf;

use gmod_fs::UserDir;
use gmod_net::{Identity, PublicKey};

/// What a `SteamID64` of an individual account starts from: the account's number is added.
const STEAM_ID64_BASE: u64 = 76561197960265728;

/// The key in a file of openGMOD's folder, made there the first time. Without a folder, or
/// when the file cannot be read or written, a key for this run only: the player is someone
/// else to servers next time.
fn identity(file: &str) -> Identity {
    let Some(path) = UserDir::default_root().map(|root| root.join(file)) else {
        eprintln!("There is no home folder to keep the key in: a new one for this run only");
        return Identity::generate();
    };
    Identity::load_or_create(&path).unwrap_or_else(|e| {
        eprintln!("{}: {e}; a key for this run only", path.display());
        Identity::generate()
    })
}

/// The player's key: who they are on every server.
pub fn player_identity() -> Identity {
    identity("identity.key")
}

/// This computer's server's key: what its players know it by.
pub fn server_identity() -> Identity {
    identity("server.key")
}

/// The `SteamID64` a key makes: an individual account's, its number from the key's hash.
pub fn steam_id64(key: &PublicKey) -> u64 {
    let digest = key.digest();
    let account = u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]]).max(1);
    STEAM_ID64_BASE + u64::from(account)
}

/// `STEAM_0:Y:Z`, as GMod writes a `SteamID64`.
pub fn steam_id(id64: u64) -> String {
    let account = id64.saturating_sub(STEAM_ID64_BASE);
    format!("STEAM_0:{}:{}", account % 2, account / 2)
}

/// The keys of the servers played on, by address.
pub struct KnownServers {
    path: Option<PathBuf>,
}

impl KnownServers {
    /// The list in openGMOD's folder.
    pub fn open() -> KnownServers {
        KnownServers { path: UserDir::default_root().map(|root| root.join("servers").join("known.txt")) }
    }

    /// The list in a file (none: nothing is known, nor kept).
    #[cfg(test)]
    pub fn at(path: Option<PathBuf>) -> KnownServers {
        KnownServers { path }
    }

    /// Each line: an address and its key.
    fn lines(&self) -> Vec<(String, PublicKey)> {
        let text = self.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| {
                let (address, key) = l.trim().split_once(char::is_whitespace)?;
                Some((address.to_string(), PublicKey::from_hex(key)?))
            })
            .collect()
    }

    /// The key the server at `address` had.
    pub fn key(&self, address: &str) -> Option<PublicKey> {
        self.lines().into_iter().find(|(a, _)| a.eq_ignore_ascii_case(address)).map(|(_, k)| k)
    }

    /// The server at `address` has `key`: kept, unless it is known already.
    pub fn remember(&self, address: &str, key: PublicKey) {
        let Some(path) = &self.path else { return };
        if self.key(address).is_some() {
            return;
        }
        let saved = (|| -> std::io::Result<()> {
            use std::io::Write;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let new = !path.exists();
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            if new {
                writeln!(file, "# The keys of the servers played on. A server whose key changed is not joined until its line goes.")?;
            }
            writeln!(file, "{address} {key}")
        })();
        if let Err(e) = saved {
            eprintln!("{}: {e}", path.display());
        }
    }

    /// What the player is told when the server at `address` has `key`, not the key it had.
    pub fn changed(&self, address: &str, key: &PublicKey) -> String {
        let had = self.key(address).map_or_else(|| "?".to_string(), |k| k.to_hex());
        let file = self.path.as_ref().map_or_else(|| "the list of known servers".to_string(), |p| p.display().to_string());
        format!(
            "{address} is not the server that was there: its key was {had}, now it is {key}. \
             Someone may stand in for it. If its owner says they changed its key, remove its line from {file}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_are_known_by_the_key_they_first_had() {
        let dir = std::env::temp_dir().join(format!("opengmod-known-servers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let known = KnownServers::at(Some(dir.join("servers").join("known.txt")));
        let (one, two) = (Identity::generate().public(), Identity::generate().public());
        assert_eq!(known.key("10.0.0.1:27015"), None);
        known.remember("10.0.0.1:27015", one);
        known.remember("10.0.0.1:27015", two);
        known.remember("10.0.0.2:27015", two);
        assert_eq!(known.key("10.0.0.1:27015"), Some(one), "the first stays");
        assert_eq!(known.key("10.0.0.2:27015"), Some(two));
        assert!(known.changed("10.0.0.1:27015", &two).contains(&one.to_hex()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_makes_the_same_steam_id_every_time() {
        let key = Identity::generate().public();
        let id = steam_id64(&key);
        assert_eq!(id, steam_id64(&key));
        assert!((STEAM_ID64_BASE + 1..=STEAM_ID64_BASE + u64::from(u32::MAX)).contains(&id));
        assert_ne!(id, steam_id64(&Identity::generate().public()));
    }
}
