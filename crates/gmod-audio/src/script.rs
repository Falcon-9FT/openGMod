//! Sound scripts: names such as `Weapon_Pistol.Single` for sounds with their settings.
//!
//! `scripts/game_sounds_manifest.txt` lists the files of scripts; each is KeyValues, a block
//! a sound:
//!
//! ```text
//! "Weapon_Pistol.Single"
//! {
//!     "channel"     "CHAN_WEAPON"
//!     "volume"      "0.55"
//!     "soundlevel"  "SNDLVL_GUNFIRE"
//!     "pitch"       "PITCH_NORM"      // or a range: "95,105"
//!     "wave"        ")weapons/pistol/pistol_fire2.wav"
//!     // or "rndwave" { "wave" "..." "wave" "..." }, one picked each time
//! }
//! ```
//!
//! A sound defined twice keeps its first definition, as the engine does.

use std::collections::HashMap;

use gmod_fs::Vfs;

/// `CHAN_AUTO`: any free channel; sounds on it never cut each other off.
pub const CHAN_AUTO: i32 = 0;
/// `CHAN_STATIC`: sounds that are never cut off either.
pub const CHAN_STATIC: i32 = 6;

/// A sound script's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    pub channel: i32,
    /// From and to, picked between each time; 1 is the sound as recorded.
    pub volume: (f32, f32),
    /// The sound level in decibels: how far it carries (75 is normal speech, 0 everywhere).
    pub level: f32,
    /// From and to, in percent of the recorded pitch.
    pub pitch: (f32, f32),
    /// The waves to pick from, as written (with Source's prefixes).
    pub waves: Vec<String>,
}

impl Default for Script {
    fn default() -> Script {
        Script { channel: CHAN_AUTO, volume: (1.0, 1.0), level: 75.0, pitch: (100.0, 100.0), waves: Vec::new() }
    }
}

/// The sound scripts of a game, by lower-case name.
#[derive(Debug, Clone, Default)]
pub struct Scripts {
    by_name: HashMap<String, Script>,
}

impl Scripts {
    /// The scripts of the files `scripts/game_sounds_manifest.txt` lists.
    pub fn load(vfs: &Vfs) -> Scripts {
        let mut scripts = Scripts::default();
        let Ok(data) = vfs.read("scripts/game_sounds_manifest.txt", "GAME") else { return scripts };
        let (document, _) = gmod_kv::parse_lenient(&gmod_kv::decode_text(&data), gmod_kv::Options::default());
        let Some(manifest) = document.root.iter().find_map(|p| p.value.as_block()) else { return scripts };
        for pair in manifest {
            if let Some(file) = pair.value.as_str()
                && let Ok(data) = vfs.read(file, "GAME")
            {
                scripts.add_file(&gmod_kv::decode_text(&data));
            }
        }
        scripts
    }

    /// Adds the scripts of the text of one file.
    pub fn add_file(&mut self, text: &str) {
        let (mut document, _) = gmod_kv::parse_lenient(text, gmod_kv::Options::default());
        document.root.apply_conditions(&gmod_kv::pc_symbol);
        for pair in &document.root {
            if let Some(block) = pair.value.as_block() {
                self.by_name.entry(pair.key.to_lowercase()).or_insert_with(|| parse(block));
            }
        }
    }

    /// The script of this name, any case.
    pub fn get(&self, name: &str) -> Option<&Script> {
        self.by_name.get(&name.to_lowercase())
    }

    /// The names of every script, lower-case.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// A script's block.
pub fn parse(block: &gmod_kv::Block) -> Script {
    let mut script = Script::default();
    if let Some(channel) = block.str("channel").and_then(channel) {
        script.channel = channel;
    }
    if let Some(volume) = block.str("volume").and_then(|v| range(v, volume)) {
        script.volume = volume;
    }
    if let Some((level, _)) = block.str("soundlevel").or_else(|| block.str("attenuation")).and_then(|v| range(v, level)) {
        script.level = level;
    }
    if let Some(pitch) = block.str("pitch").and_then(|v| range(v, pitch)) {
        script.pitch = pitch;
    }
    for pair in block {
        if pair.key.eq_ignore_ascii_case("wave") {
            script.waves.extend(pair.value.as_str().map(str::to_string));
        } else if pair.key.eq_ignore_ascii_case("rndwave")
            && let Some(list) = pair.value.as_block()
        {
            script.waves.extend(list.iter().filter_map(|p| p.value.as_str().map(str::to_string)));
        }
    }
    script
}

/// `CHAN_*` by name, or a number.
pub fn channel(text: &str) -> Option<i32> {
    let text = text.trim();
    let named = match text.to_ascii_uppercase().as_str() {
        "CHAN_REPLACE" => -1,
        "CHAN_AUTO" => 0,
        "CHAN_WEAPON" => 1,
        "CHAN_VOICE" => 2,
        "CHAN_ITEM" => 3,
        "CHAN_BODY" => 4,
        "CHAN_STREAM" => 5,
        "CHAN_STATIC" => 6,
        "CHAN_VOICE2" => 7,
        "CHAN_VOICE_BASE" => 8,
        "CHAN_USER_BASE" => 136,
        _ => return text.parse().ok(),
    };
    Some(named)
}

/// A volume: `VOL_NORM` or a number.
pub fn volume(text: &str) -> Option<f32> {
    if text.eq_ignore_ascii_case("VOL_NORM") { Some(1.0) } else { text.parse().ok() }
}

/// A sound level: `SNDLVL_NORM`, `SNDLVL_80dB` or a number of decibels.
pub fn level(text: &str) -> Option<f32> {
    let upper = text.to_ascii_uppercase();
    let named = match upper.as_str() {
        "SNDLVL_NONE" => 0.0,
        "SNDLVL_IDLE" | "SNDLVL_TALKING" => 60.0,
        "SNDLVL_STATIC" => 66.0,
        "SNDLVL_NORM" => 75.0,
        "SNDLVL_GUNFIRE" => 140.0,
        _ => {
            let db = upper.strip_prefix("SNDLVL_").and_then(|r| r.strip_suffix("DB"));
            return db.unwrap_or(&upper).parse().ok();
        }
    };
    Some(named)
}

/// A pitch in percent: `PITCH_NORM`, `PITCH_LOW`, `PITCH_HIGH` or a number.
pub fn pitch(text: &str) -> Option<f32> {
    match text.to_ascii_uppercase().as_str() {
        "PITCH_NORM" => Some(100.0),
        "PITCH_LOW" => Some(95.0),
        "PITCH_HIGH" => Some(120.0),
        _ => text.parse().ok(),
    }
}

/// One value or a range `"from, to"`, each read by `one`.
pub fn range(text: &str, one: fn(&str) -> Option<f32>) -> Option<(f32, f32)> {
    let mut parts = text.split(',').map(str::trim);
    let from = one(parts.next()?)?;
    let to = parts.next().and_then(one).unwrap_or(from);
    Some((from.min(to), from.max(to)))
}

/// The file of a wave as a game path (`sound/...`), without the characters Source puts in
/// front of wave names to say how to play them (`)`, `^`, `*`, `#`, `@`, `<`, `>`...).
pub fn wave_path(wave: &str) -> String {
    let name = wave.trim_start_matches(['*', '#', '@', '<', '>', '^', ')', '(', '}', '$', '!', '?', '&', '~', '`', '+', '%']);
    let name = name.replace('\\', "/");
    let name = name.trim_start_matches('/');
    if name.len() > 6 && name[..6].eq_ignore_ascii_case("sound/") { name.to_string() } else { format!("sound/{name}") }
}
