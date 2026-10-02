//! The game in the window: the Lua of the server - `includes/init.lua`, autorun, the `sandbox`
//! gamemode and the addons, as Garry's Mod starts them - running the map, and the player of a
//! single-player game moved through it by the keys and the mouse. What the game prints goes to
//! the terminal, and lines typed there are the player's console (`give weapon_crowbar`,
//! `gm_spawn models/props_c17/oildrum001.mdl`, `lua_run`, the commands of the addons). The
//! models of the entities the player sees are read the first time one shows ([`Models`]).

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use gmod_fs::Vfs;
use gmod_lua::player::{IMPULSE_FLASHLIGHT, key};
use gmod_lua::vector::{Angle, Vector};
use gmod_lua::{Drawn, EntityHandle, Map, Output, PlayerInfo, PlayerView, Realm, Runtime, UserCmd};
use gmod_movement::FULL_MOVE;
use gmod_render::{ModelId, Placed};
use gmod_world::World;

use crate::walk::Keys;

/// What the player does besides walking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons {
    pub attack: bool,
    pub attack2: bool,
    pub reload: bool,
    pub using: bool,
    /// Walking slowly (Alt).
    pub walk: bool,
}

/// The command the keys make, looking along `pitch` and `yaw`: how the player moves and the
/// `IN_` keys held.
pub fn command(keys: &Keys, buttons: &Buttons, pitch: f32, yaw: f32) -> UserCmd {
    let axis = |plus: bool, minus: bool| (f32::from(plus) - f32::from(minus)) * FULL_MOVE;
    let held = [
        (keys.forward, key::FORWARD),
        (keys.back, key::BACK),
        (keys.left, key::MOVELEFT),
        (keys.right, key::MOVERIGHT),
        (keys.jump, key::JUMP),
        (keys.duck, key::DUCK),
        (keys.run, key::SPEED),
        (buttons.walk, key::WALK),
        (buttons.attack, key::ATTACK),
        (buttons.attack2, key::ATTACK2),
        (buttons.reload, key::RELOAD),
        (buttons.using, key::USE),
    ];
    UserCmd {
        view_angles: Angle::new(pitch, yaw, 0.0),
        forward_move: axis(keys.forward, keys.back),
        side_move: axis(keys.right, keys.left),
        buttons: held.iter().filter(|(on, _)| *on).fold(0, |all, (_, bit)| all | bit),
        ..UserCmd::default()
    }
}

/// What a frame shows.
pub struct Shown {
    /// Where to see from: between where the eyes were at the last two ticks.
    pub eye: [f32; 3],
    /// The game turned the player itself (a spawn, `SetEyeAngles`): the camera's pitch and
    /// yaw from now on.
    pub turned: Option<(f32, f32)>,
    pub view: PlayerView,
}

pub struct Game {
    runtime: Runtime,
    player: EntityHandle,
    console: Receiver<String>,
    /// The impulse for the next command (the flashlight).
    impulse: u8,
}

impl Game {
    /// Starts the Lua of the server on the game's files with the map, and lets the player join.
    pub fn start(vfs: Arc<Vfs>, map: &Map) -> Result<Game, String> {
        let runtime = Runtime::new(Realm::Server, vfs).map_err(|e| format!("LuaJIT could not be started: {e}"))?;
        runtime.set_output(|output| match output {
            Output::Text(text) => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            Output::Error(error) => eprintln!("{error}"),
        });
        if let Some(dir) = gmod_fs::UserDir::default_root().and_then(|root| gmod_fs::UserDir::open(root, None).ok()) {
            runtime.set_data_dir(dir);
        }
        runtime.start("sandbox");
        runtime.spawn_map(map);
        let player = runtime.join(PlayerInfo { name: "Player".into(), ..PlayerInfo::default() }).ok_or("no player could join the game")?;
        let errors = runtime.errors().len();
        println!("Lua: {} files, {errors} errors; type commands here (give weapon_crowbar, gm_spawn <model>)", runtime.files_loaded());
        Ok(Game { runtime, player, console: console(), impulse: 0 })
    }

    /// Runs a console command as the player.
    pub fn command(&self, line: &str) {
        self.runtime.run_player_command(self.player, line);
    }

    /// The flashlight, on or off, with the next command.
    pub fn flashlight(&mut self) {
        self.impulse = IMPULSE_FLASHLIGHT;
    }

    /// One frame of `seconds`: the lines typed into the terminal, then the ticks of the game
    /// with the command the keys make.
    pub fn frame(&mut self, keys: &Keys, buttons: &Buttons, pitch: f32, yaw: f32, seconds: f64) -> Option<Shown> {
        while let Ok(line) = self.console.try_recv() {
            self.command(&line);
        }
        let mut cmd = command(keys, buttons, pitch, yaw);
        cmd.impulse = std::mem::take(&mut self.impulse);
        self.runtime.set_input(self.player, cmd);
        self.runtime.think(seconds);
        let view = self.runtime.view(self.player)?;
        let eye = between(view.previous_eye, view.eye, self.runtime.tick_fraction());
        let turned = (angle_apart(view.angles.p, pitch) || angle_apart(view.angles.y, yaw)).then_some((view.angles.p, view.angles.y));
        Some(Shown { eye, turned, view })
    }

    /// The models of the entities the player sees.
    pub fn drawn(&self) -> Vec<Drawn> {
        self.runtime.drawn(self.player)
    }
}

/// The models of the game's entities, each read from the game's files the first time an entity
/// has it, and given to the renderer.
pub struct Models {
    vfs: Arc<Vfs>,
    /// By name and skin; none for those that could not be read.
    loaded: HashMap<(String, i64), Option<ModelId>>,
}

impl Models {
    pub fn new(vfs: Arc<Vfs>) -> Models {
        Models { vfs, loaded: HashMap::new() }
    }

    /// Where to draw the models of `drawn`. Those not seen before are read and handed to
    /// `upload`; one that cannot be read is told once and not drawn.
    pub fn place(&mut self, drawn: &[Drawn], mut upload: impl FnMut(&World) -> ModelId) -> Vec<Placed> {
        drawn
            .iter()
            .filter_map(|d| {
                let key = (d.model.to_ascii_lowercase().replace('\\', "/"), d.skin);
                let model = *self.loaded.entry(key).or_insert_with_key(|(name, skin)| {
                    match gmod_world::model(&self.vfs, name, usize::try_from(*skin).unwrap_or(0)) {
                        Ok(model) => Some(upload(&model)),
                        Err(e) => {
                            println!("{name}: {e}");
                            None
                        }
                    }
                });
                let (o, a) = (d.origin, d.angles);
                Some(Placed { model: model?, origin: [o.x, o.y, o.z], angles: [a.p, a.y, a.r], scale: d.scale })
            })
            .collect()
    }
}

fn between(from: Vector, to: Vector, t: f32) -> [f32; 3] {
    let at = from + (to - from) * t;
    [at.x, at.y, at.z]
}

/// Whether two angles in degrees differ, a turn apart counting as the same.
fn angle_apart(a: f32, b: f32) -> bool {
    ((a - b + 180.0).rem_euclid(360.0) - 180.0).abs() > 0.01
}

/// Lines typed into the terminal, read on a thread of their own.
fn console() -> Receiver<String> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    receive
}

/// The title of the window while the game runs: health and the weapon in hand.
pub fn status(view: &PlayerView) -> String {
    if !view.alive {
        return "dead - click to spawn again".to_string();
    }
    let mut status = format!("health {}", view.health);
    if view.armor > 0 {
        status.push_str(&format!(", armor {}", view.armor));
    }
    if let Some(weapon) = &view.weapon {
        status.push_str(&format!(" - {}", weapon.class));
        match (weapon.clip, weapon.ammo) {
            (clip, ammo) if clip >= 0 => status.push_str(&format!(" {clip}/{ammo}")),
            (_, ammo) if ammo > 0 => status.push_str(&format!(" {ammo}")),
            _ => {}
        }
    }
    status
}

#[cfg(test)]
mod tests {
    use gmod_bsp::testmap::{TestBrush, TestMap};
    use gmod_bsp::{Bsp, Collision};
    use gmod_fs::{Backing, DirMount, Mount};

    use super::*;

    /// What the game's Lua does in [`the_game_runs_with_the_player_in_it`]: the hooks of a
    /// gamemode, and a console command that sets the player's health.
    const INIT: &str = "
        HOOKS = {}
        hook = { Call = function(name, gm, ...) if HOOKS[name] then return HOOKS[name](...) end end }
        concommand = { Run = function(p, cmd, args) if cmd == 'health' then p:SetHealth(tonumber(args[1])) end end }
        AddConsoleCommand('health')
        HOOKS.PlayerSelectSpawn = function(p) return ents.FindByClass('info_player_start')[1] end
        HOOKS.PlayerNoClip = function(p, on) return true end
    ";

    #[test]
    fn the_game_runs_with_the_player_in_it() {
        let dir = std::env::temp_dir().join(format!("opengmod-game-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lua/includes")).unwrap();
        std::fs::write(dir.join("lua/includes/init.lua"), INIT).unwrap();
        let mut vfs = Vfs::new();
        vfs.push(Mount::new(&["GAME"], Backing::Dir(DirMount::new(&dir))));
        let floor =
            TestMap { models: vec![vec![TestBrush::cuboid([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0])]], ..TestMap::default() };
        let bsp = Bsp::parse("gm_test.bsp".into(), Arc::from(floor.bytes())).unwrap();
        let mut map = Map::from_bsp("gm_test", &bsp, Arc::new(Collision::new(&bsp)));
        let start = [("classname", "info_player_start"), ("origin", "0 0 0"), ("angles", "0 90 0")];
        map.entities.push(start.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
        let mut game = Game::start(Arc::new(vfs), &map).unwrap();

        // the camera turns to where the spawn point looks, and sees from the player's eyes
        let still = (Keys::default(), Buttons::default());
        let shown = game.frame(&still.0, &still.1, 0.0, 0.0, 0.0).unwrap();
        assert_eq!(shown.turned, Some((0.0, 90.0)));
        assert!((shown.eye[2] - 64.0).abs() < 0.1, "{:?}", shown.eye);

        // walking along y for half a second, drawn between the ticks
        let forward = Keys { forward: true, ..Keys::default() };
        let mut shown = shown;
        for _ in 0..30 {
            shown = game.frame(&forward, &still.1, 0.0, 90.0, 1.0 / 60.0).unwrap();
        }
        assert!(shown.eye[1] > 50.0 && shown.eye[0].abs() < 0.1 && shown.turned.is_none(), "{:?}", shown.eye);

        // the console: the game's commands and those of Lua, as the player
        game.command("health 42; noclip");
        let shown = game.frame(&still.0, &still.1, -60.0, 90.0, 0.1).unwrap();
        assert_eq!(shown.view.health, 42);
        let before = shown.eye;
        let shown = game.frame(&forward, &still.1, -60.0, 90.0, 0.5).unwrap();
        assert!(shown.eye[2] > before[2] + 50.0, "noclip flies where the player looks: {before:?} {:?}", shown.eye);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn models_are_read_once() {
        let mut models = Models::new(Arc::new(Vfs::new()));
        let drawn = Drawn {
            entity: EntityHandle::NULL,
            model: "models/props_c17/oildrum001.mdl".into(),
            skin: 0,
            origin: Vector::new(1.0, 2.0, 3.0),
            angles: Angle::new(0.0, 90.0, 0.0),
            scale: 1.0,
        };
        // not in the game's files: not drawn, and not looked for again
        assert!(models.place(&[drawn.clone(), drawn.clone()], |_| unreachable!("nothing to upload")).is_empty());
        assert_eq!(models.loaded.len(), 1);
        assert_eq!(models.loaded.get(&(drawn.model.clone(), 0)), Some(&None));
    }

    #[test]
    fn keys_make_a_command() {
        let keys = Keys { forward: true, left: true, run: true, ..Keys::default() };
        let buttons = Buttons { attack: true, using: true, ..Buttons::default() };
        let cmd = command(&keys, &buttons, 10.0, 90.0);
        assert_eq!(cmd.view_angles, Angle::new(10.0, 90.0, 0.0));
        assert_eq!((cmd.forward_move, cmd.side_move), (FULL_MOVE, -FULL_MOVE));
        assert_eq!(cmd.buttons, key::FORWARD | key::MOVELEFT | key::SPEED | key::ATTACK | key::USE);
    }

    #[test]
    fn angles_a_turn_apart_are_the_same() {
        assert!(!angle_apart(-90.0, 270.0));
        assert!(!angle_apart(450.0, 90.0));
        assert!(angle_apart(10.0, 11.0));
    }

    #[test]
    fn the_title_tells_health_and_ammo() {
        let mut view = PlayerView {
            eye: Vector::default(),
            previous_eye: Vector::default(),
            angles: Angle::default(),
            fov: 75.0,
            alive: true,
            health: 80,
            armor: 0,
            weapon: Some(gmod_lua::WeaponView { class: "weapon_pistol".into(), name: "#HL2_Pistol".into(), clip: 12, ammo: 36 }),
        };
        assert_eq!(status(&view), "health 80 - weapon_pistol 12/36");
        view.weapon = None;
        view.armor = 15;
        assert_eq!(status(&view), "health 80, armor 15");
        view.alive = false;
        assert_eq!(status(&view), "dead - click to spawn again");
    }
}
