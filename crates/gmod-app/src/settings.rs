//! The player's settings and key bindings, as GMod keeps them in `cfg/config.cfg`
//! (`sensitivity "3"`, `bind "w" "+forward"`): in openGMOD's own folder
//! (`~/.opengmod/cfg/config.cfg`). The first time, they are what the game's
//! `cfg/config_default.cfg` and the player's own Garry's Mod `cfg/config.cfg` say - read, never
//! written. The options window changes them ([`gmod_lua::SettingChange`]); they are kept at
//! once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The settings there are, and what each is when nothing says otherwise.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("sensitivity", "3"),
    ("m_pitch", "0.022"),
    ("m_yaw", "0.022"),
    ("fov_desired", "75"),
    ("volume", "1"),
    ("snd_musicvolume", "1"),
    ("fps_max", "300"),
    ("mat_vsync", "1"),
    ("opengmod_fullscreen", "0"),
    ("opengmod_width", "1280"),
    ("opengmod_height", "720"),
    ("cl_drawhud", "1"),
    ("cl_showfps", "0"),
    ("crosshair", "1"),
    ("r_drawviewmodel", "1"),
    // the language of the game and its menu (the main menu's flag chooses it)
    ("gmod_language", "en"),
    // the options dialog's, as GameUI's pages have them
    ("cl_downloadfilter", "all"),
    ("cl_enable_loadingurl", "1"),
    ("hud_fastswitch", "0"),
    ("cl_logofile", "materials/vgui/logos/spray.vtf"),
    ("cl_playerspraydisable", "0"),
    ("m_filter", "0"),
    ("m_rawinput", "1"),
    ("m_customaccel", "0"),
    ("m_customaccel_exponent", "1.05"),
    ("joystick", "0"),
    ("joy_inverty", "0"),
    ("joy_movement_stick", "0"),
    ("joy_yawsensitivity", "-0.5"),
    ("joy_pitchsensitivity", "0.5"),
    ("opengmod_sfx_volume", "1"),
    ("snd_surround_speakers", "2"),
    ("opengmod_sound_quality", "2"),
    ("closecaption", "0"),
    ("cc_subtitles", "0"),
    ("snd_mute_losefocus", "1"),
    ("dsp_enhance_stereo", "0"),
    ("voice_enable", "1"),
    ("voice_scale", "1"),
    ("opengmod_mic_volume", "1"),
    ("opengmod_mic_boost", "1"),
    ("r_rootlod", "0"),
    ("mat_picmip", "0"),
    ("mat_reducefillrate", "0"),
    ("opengmod_water_detail", "2"),
    // the graphics API to draw with: "dx12", "vulkan", or "auto" (the best the GPU has)
    ("opengmod_renderer", "auto"),
    ("opengmod_shadow_detail", "2"),
    ("mat_colorcorrection", "1"),
    ("mat_antialias", "1"),
    ("mat_forceaniso", "8"),
    ("mat_motion_blur_enabled", "0"),
    ("mat_queue_mode", "-1"),
    ("mat_hdr_level", "2"),
    ("mat_disable_d3d9ex", "0"),
];

/// The keys as GMod's `cfg/config_default.cfg` binds them: for a player without Garry's Mod's
/// own files to read them from.
const DEFAULT_BINDS: &str = "bind w +forward; bind s +back; bind a +moveleft; bind d +moveright; bind SPACE +jump; bind CTRL +duck; bind SHIFT +speed; bind ALT +walk; bind TAB +showscores; bind MOUSE1 +attack; bind MOUSE2 +attack2; bind MWHEELUP invprev; bind MWHEELDOWN invnext; bind ` toggleconsole; bind F10 toggleconsole; bind F5 jpeg; bind 1 slot1; bind 2 slot2; bind 3 slot3; bind 4 slot4; bind 5 slot5; bind 6 slot6; bind q +menu; bind e +use; bind r +reload; bind y messagemode; bind u messagemode2; bind f \"impulse 100\"; bind z gmod_undo; bind c +menu_context; bind v noclip; bind b +zoom; bind x +voicerecord";

/// The settings the game's Lua sees as console variables of the client.
pub const CLIENT_CONVARS: &[&str] = &[
    "sensitivity",
    "m_pitch",
    "m_yaw",
    "fov_desired",
    "cl_drawhud",
    "cl_showfps",
    "crosshair",
    "r_drawviewmodel",
    "snd_musicvolume",
    "volume",
    "gmod_language",
];

/// The player's settings and key bindings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    values: BTreeMap<String, String>,
    /// Key name (lower case) to command, in the order they were bound.
    binds: Vec<(String, String)>,
    /// Where they are kept; none: nowhere (tests).
    path: Option<PathBuf>,
}

/// The words of a config line: quoted or not, `//` ending it.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            out.push(chars.by_ref().take_while(|&c| c != '"').collect());
        } else if c == '/' && line.contains("//") && chars.clone().nth(1) == Some('/') {
            break;
        } else {
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                word.push(c);
                chars.next();
            }
            out.push(word);
        }
    }
    out
}

impl Config {
    /// The settings kept, or - the first time - those of the game's defaults and the player's
    /// own Garry's Mod (`game_dir`: its `garrysmod` folder), then kept.
    pub fn load(game_dir: Option<&Path>) -> Config {
        let path = gmod_fs::UserDir::default_root().map(|root| root.join("cfg").join("config.cfg"));
        let mut config = Config { path: path.clone(), ..Config::defaults() };
        match path.as_deref().and_then(|p| std::fs::read(p).ok()) {
            Some(kept) => {
                let kept = String::from_utf8_lossy(&kept).into_owned();
                config.read(&kept);
                // settings openGMOD did not keep yet (they came later): as Garry's Mod has them
                let mut gmod = Config::defaults();
                for file in ["cfg/config_default.cfg", "cfg/config.cfg"] {
                    if let Some(text) = game_dir.and_then(|dir| std::fs::read(dir.join(file)).ok()) {
                        gmod.read(&String::from_utf8_lossy(&text));
                    }
                }
                let mentioned = |name: &str| kept.lines().any(|l| words(l).first().is_some_and(|w| w.eq_ignore_ascii_case(name)));
                for (name, _) in DEFAULTS.iter().filter(|(name, _)| !mentioned(name)) {
                    config.set(name, gmod.value(name));
                }
            }
            None => {
                config.read(DEFAULT_BINDS);
                for file in ["cfg/config_default.cfg", "cfg/config.cfg"] {
                    if let Some(text) = game_dir.and_then(|dir| std::fs::read(dir.join(file)).ok()) {
                        config.read(&String::from_utf8_lossy(&text));
                    }
                }
                config.save();
            }
        }
        config
    }

    /// Every setting at its default, no key bound.
    pub fn defaults() -> Config {
        Config { values: DEFAULTS.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), ..Config::default() }
    }

    /// Takes in a config file's lines: `bind`, `unbind`, `unbindall`, and the settings there
    /// are; the rest is not the program's.
    pub fn read(&mut self, text: &str) {
        for line in text.lines() {
            for command in line.split(';') {
                let words = words(command);
                match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                    ["unbindall"] => self.binds.clear(),
                    [bind, key, command, ..] if bind.eq_ignore_ascii_case("bind") => self.bind(key, Some(command)),
                    [unbind, key] if unbind.eq_ignore_ascii_case("unbind") => self.bind(key, None),
                    [name, value, ..] if DEFAULTS.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) => {
                        self.values.insert(name.to_ascii_lowercase(), value.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    /// A setting's value; empty for one there is not.
    pub fn value(&self, name: &str) -> &str {
        self.values.get(&name.to_ascii_lowercase()).map_or("", String::as_str)
    }

    /// A setting as a number, `default` when it is none.
    pub fn number(&self, name: &str, default: f32) -> f32 {
        self.value(name).trim().parse().unwrap_or(default)
    }

    /// Every setting and its value.
    pub fn values(&self) -> BTreeMap<String, String> {
        self.values.clone()
    }

    /// Sets a setting.
    pub fn set(&mut self, name: &str, value: &str) {
        self.values.insert(name.to_ascii_lowercase(), value.to_string());
    }

    /// A key (its engine name) does `command` from now on; none: nothing.
    pub fn bind(&mut self, key: &str, command: Option<&str>) {
        let key = key.to_lowercase();
        self.binds.retain(|(k, _)| *k != key);
        if let Some(command) = command.filter(|c| !c.is_empty()) {
            self.binds.push((key, command.to_string()));
        }
    }

    /// What a key (its engine name) does.
    pub fn command(&self, key: &str) -> Option<&str> {
        self.binds.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, c)| c.as_str())
    }

    /// Every key bound, and what it does.
    pub fn binds(&self) -> &[(String, String)] {
        &self.binds
    }

    /// The file as GMod writes `config.cfg`.
    pub fn text(&self) -> String {
        let mut out = String::from("unbindall\n");
        for (key, command) in &self.binds {
            out.push_str(&format!("bind \"{key}\" \"{command}\"\n"));
        }
        for (name, value) in &self.values {
            out.push_str(&format!("{name} \"{value}\"\n"));
        }
        out
    }

    /// Keeps them, where they are kept.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, self.text()) {
            say_error!("{}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_gmod_the_keys_are_gmods_own() {
        let mut config = Config::defaults();
        config.read(DEFAULT_BINDS);
        assert_eq!(config.command("w"), Some("+forward"));
        assert_eq!(config.command("f"), Some("impulse 100"));
        assert_eq!(config.command("`"), Some("toggleconsole"));
        assert_eq!(config.command("mouse1"), Some("+attack"));
    }

    #[test]
    fn a_config_of_gmod_is_read_and_written_back() {
        let mut config = Config::defaults();
        config.read(
            "unbindall\nbind \"w\" \"+forward\"\nbind \"MOUSE1\" \"+attack\" // shoot\nbind q +menu; bind e \"+use\"\n\
             sensitivity \"2.5\"\nm_pitch \"-0.022\"\nsomething_else \"1\"\n",
        );
        assert_eq!(config.command("W"), Some("+forward"));
        assert_eq!(config.command("mouse1"), Some("+attack"));
        assert_eq!(config.command("q"), Some("+menu"));
        assert_eq!(config.command("e"), Some("+use"));
        assert_eq!(config.number("sensitivity", 3.0), 2.5);
        assert!(config.number("m_pitch", 0.022) < 0.0);
        assert_eq!(config.value("something_else"), "", "not the program's");
        config.bind("w", None);
        config.bind("up", Some("+forward"));
        let mut again = Config::defaults();
        again.read(&config.text());
        assert_eq!(again.command("w"), None);
        assert_eq!(again.command("up"), Some("+forward"));
        assert_eq!(again.values(), config.values());
    }
}
