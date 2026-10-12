//! The game in the window: a server - the Lua of the server, `includes/init.lua`, autorun,
//! the gamemode and the addons, as Garry's Mod starts them, running the map ([`Server`]) - and
//! the player's client: the client's Lua, which has what the server has of the entities every
//! frame, draws the HUD and the menus (Q, C), takes the keys and the mouse first, and sends the
//! server what it asks for (`gm_spawn` from the spawn menu, the tool chosen). The server runs in
//! this program - a single-player game, or a listen server others join - or elsewhere: then the
//! client hears it over the network ([`Connection`]) and draws what it sends. What the game
//! prints goes to the terminal, and lines typed there are the player's console (`give
//! weapon_crowbar`, `gm_spawn models/props_c17/oildrum001.mdl`, `lua_run`, the commands of the
//! addons). The models of the entities the player sees are read the first time one shows
//! ([`Models`]).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};

use gmod_fs::Vfs;
use gmod_lua::player::{IMPULSE_FLASHLIGHT, key};
use gmod_lua::vector::{Angle, Vector};
use gmod_lua::{Drawn, EntityHandle, Map, PlayerView, Realm, Runtime, UserCmd};
use gmod_movement::FULL_MOVE;
use gmod_render::{ModelId, Placed};
use gmod_world::World;

use crate::client::{Connection, Event};
use crate::proto::ToServer;
use crate::server::{ForHost, Server};
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
    /// Notches the mouse wheel turned since the last command, up positive.
    pub wheel: i32,
    /// How far the mouse moved since the last command, right and down, in its own counts.
    pub mouse: [i32; 2],
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
        mouse_wheel: buttons.wheel,
        mouse_x: buttons.mouse[0],
        mouse_y: buttons.mouse[1],
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

/// Where the client's server is.
enum Link {
    /// In this program: the client sees its entities directly.
    Local(Box<Server>),
    /// Over the network.
    Remote(Box<Connection>),
}

pub struct Game {
    link: Link,
    /// The player's client.
    client: Runtime,
    player: EntityHandle,
    /// The impulse for the next command (the flashlight).
    impulse: u8,
    /// The turns of the player by the game the camera has taken ([`PlayerView::turned`]).
    turned: Option<u32>,
    /// The server's sounds for the client since the last frame.
    sounds: Vec<gmod_lua::sound::SoundEvent>,
    /// Console lines typed for the next frame.
    typed: Vec<String>,
}

/// The player's client, on the game's files with the map, for `max_players`.
fn client(vfs: Arc<Vfs>, map: &Map, gamemode: &str, max_players: u32, pages: Option<gmod_lua::WebPages>) -> Result<Runtime, String> {
    let client = crate::server::runtime(Realm::Client, vfs)?;
    if let Some(pages) = pages {
        client.set_web_pages(pages);
    }
    client.set_max_players(max_players);
    client.start(gamemode);
    client.spawn_map(map);
    Ok(client)
}

impl Game {
    /// Starts the Lua of the server on the game's files with the map for `max_players`, lets
    /// the player join, and - with `listen` - takes others from the network.
    pub fn start(
        vfs: Arc<Vfs>,
        map: &Map,
        gamemode: &str,
        pages: Option<gmod_lua::WebPages>,
        max_players: u32,
        listen: Option<SocketAddr>,
        (name, key): (&str, gmod_net::PublicKey),
    ) -> Result<Game, String> {
        let mut server = Server::start(vfs.clone(), map, gamemode, max_players, false)?;
        if let Some(addr) = listen {
            match server.listen(addr, crate::keys::server_identity()) {
                Ok(at) => println!("Others join at {at} (opengmod --connect <this computer's address>:{})", at.port()),
                Err(e) => println!("Others cannot join: {addr}: {e}"),
            }
        }
        let player = server.join_host(name, Some(key)).ok_or("no player could join the game")?;
        let client = client(vfs, map, gamemode, max_players, pages)?;
        client.set_local_player(player);
        client.apply_snapshot(&server.runtime().snapshot());
        let errors = server.runtime().errors().len() + client.errors().len();
        println!(
            "Lua: {} files, {errors} errors; type commands here (give weapon_crowbar, gm_spawn <model>)",
            server.runtime().files_loaded() + client.files_loaded()
        );
        let mut game =
            Game { link: Link::Local(Box::new(server)), client, player, impulse: 0, turned: None, sounds: Vec::new(), typed: Vec::new() };
        game.send_client_commands();
        Ok(game)
    }

    /// Plays on a server over the network: the client starts on the game's files (with what
    /// the server sent) and the map, and the player joins.
    pub fn join(vfs: Arc<Vfs>, map: &Map, mut connection: Connection, pages: Option<gmod_lua::WebPages>) -> Result<Game, String> {
        let welcome = connection.welcome.clone();
        let client = client(vfs, map, &welcome.gamemode, welcome.max_players, pages)?;
        let player = connection.join()?;
        client.set_local_player(player);
        println!("Lua: {} files, {} errors", client.files_loaded(), client.errors().len());
        let mut game = Game {
            link: Link::Remote(Box::new(connection)),
            client,
            player,
            impulse: 0,
            turned: None,
            sounds: Vec::new(),
            typed: Vec::new(),
        };
        game.send_client_commands();
        Ok(game)
    }

    /// Whether the player may run Lua of their own: in their own game, or on a server with
    /// `sv_allowcslua`.
    pub fn own_lua_allowed(&self) -> bool {
        match &self.link {
            Link::Local(_) => true,
            Link::Remote(connection) => connection.welcome.allow_cs_lua,
        }
    }

    /// Whether others play too: the game goes on under the menu.
    pub fn multiplayer(&self) -> bool {
        match &self.link {
            Link::Local(server) => server.listening(),
            Link::Remote(_) => true,
        }
    }

    /// Why the game is over, when the server closed the connection.
    pub fn closed(&self) -> Option<&str> {
        match &self.link {
            Link::Remote(connection) => connection.closed.as_deref(),
            Link::Local(_) => None,
        }
    }

    /// Leaves the game: the server's clients are told it goes, or the server that the player
    /// does.
    pub fn leave(&mut self) {
        match &mut self.link {
            Link::Local(server) => server.shut_down("the server is going"),
            Link::Remote(connection) => connection.close("the player left"),
        }
    }

    /// What the client asked of the server since the last time: its user info, its commands,
    /// its `net` messages.
    fn send_client_commands(&mut self) {
        let info = self.client.take_user_info();
        let commands = self.client.take_server_commands();
        let messages = self.client.take_net_messages();
        match &mut self.link {
            Link::Local(server) => {
                let runtime = server.runtime();
                for (name, value) in info {
                    runtime.set_user_info(self.player, &name, &value);
                }
                for line in commands {
                    runtime.run_player_command(self.player, &line);
                }
                for message in messages {
                    runtime.receive_net_message(&message.name, &message.data, self.player);
                }
            }
            Link::Remote(connection) => {
                for (name, value) in info {
                    connection.send(&ToServer::UserInfo(name, value), true);
                }
                for line in commands {
                    connection.send(&ToServer::Command(line), true);
                }
                for message in messages {
                    connection.send(&ToServer::Net { name: message.name, data: message.data }, !message.unreliable);
                }
            }
        }
    }

    /// A key or mouse button of the game's (`KEY_*`, `MOUSE_*`) went down or up: whether the
    /// client took it - a menu, a text entry, a command of its own (`+menu`).
    pub fn button(&self, code: i32, down: bool) -> bool {
        self.client.button(code, down)
    }

    /// Text typed, for the chat box or the text entry that has the keyboard: whether one took it.
    pub fn type_text(&self, text: &str) -> bool {
        self.client.type_text(text)
    }

    /// Whether the chat box is open: Esc closes it rather than opening the main menu.
    pub fn chat_open(&self) -> bool {
        self.client.chat_open()
    }

    /// The cursor moved over the window, to a pixel of it.
    pub fn cursor_moved(&self, x: i32, y: i32) {
        self.client.set_cursor(x, y);
    }

    /// Whether the client shows the cursor (a menu is open): the mouse does not turn the view.
    pub fn cursor_visible(&self) -> bool {
        self.client.cursor_visible()
    }

    /// Draws the client's 2D for a screen of this size: the HUD, the menus.
    pub fn paint(&self, width: u32, height: u32, seconds: f64) {
        self.client.set_screen_size(width, height);
        self.client.paint(seconds);
    }

    /// What the server and the client played since the last call: sounds of entities, of
    /// places, of the menus.
    pub fn take_sounds(&mut self) -> Vec<gmod_lua::sound::SoundEvent> {
        let mut sounds = std::mem::take(&mut self.sounds);
        sounds.extend(self.client.take_sounds());
        sounds
    }

    /// The client's `DynamicLight`s that shine now.
    pub fn dynamic_lights(&self) -> Vec<gmod_lua::client::DynamicLight> {
        self.client.dynamic_lights()
    }

    /// Where an entity is now, as the client has it.
    pub fn entity_position(&self, entity: gmod_lua::entity::EntityHandle) -> Option<Vector> {
        self.client.entity_position(entity)
    }

    /// The client's runtime: what [`paint`](Self::paint) drew, and its images.
    pub fn client(&self) -> &Runtime {
        &self.client
    }

    /// Runs a console command as the player: on the server in this program, or through the
    /// client's console, which sends the server what it does not know itself.
    pub fn command(&self, line: &str) {
        match &self.link {
            Link::Local(server) => server.runtime().run_player_command(self.player, line),
            Link::Remote(_) => self.client.run_command(line),
        }
    }

    /// Console lines typed into the terminal, run as the player's with the next frame.
    pub fn type_lines(&mut self, lines: Vec<String>) {
        self.typed.extend(lines);
    }

    /// The flashlight, on or off, with the next command.
    pub fn flashlight(&mut self) {
        self.impulse = IMPULSE_FLASHLIGHT;
    }

    /// One frame of `seconds`: the command the keys make, the lines typed ([`type_lines`](Self::type_lines);
    /// after it, so that what they do to the player - `SetEyeAngles` - stands), then the
    /// ticks of the game - here, or on the server, whose entities come as they come - and
    /// the client's frame.
    pub fn frame(&mut self, keys: &Keys, buttons: &Buttons, pitch: f32, yaw: f32, seconds: f64) -> Option<Shown> {
        let mut cmd = command(keys, buttons, pitch, yaw);
        cmd.impulse = std::mem::take(&mut self.impulse);
        let lines = std::mem::take(&mut self.typed);
        match &mut self.link {
            Link::Local(server) => {
                server.runtime().set_input(self.player, cmd);
            }
            Link::Remote(connection) => connection.command(cmd, seconds as f32),
        }
        self.send_client_commands();
        for line in lines {
            self.command(&line);
        }
        let eye = match &mut self.link {
            Link::Local(server) => {
                let host = server.frame(seconds);
                // the client has what the server has now - its entities, what is said to its
                // player - and thinks
                self.client.apply_snapshot(&server.runtime().snapshot());
                deliver(&self.client, host, &mut self.sounds);
                let view = self.client.view(self.player)?;
                between(view.previous_eye, view.eye, server.runtime().tick_fraction())
            }
            Link::Remote(connection) => {
                let events = connection.poll();
                if let Some(mut snapshot) = connection.shown(seconds) {
                    // the player itself where the server last said, not a while ago
                    if let Some(own) = connection.newest(self.player)
                        && let Some(at) = snapshot.entities.iter_mut().find(|e| e.index == own.index && e.serial == own.serial)
                    {
                        *at = own.clone();
                    }
                    self.client.apply_snapshot(&snapshot);
                }
                for event in events {
                    match event {
                        Event::Chat(message) => self.client.receive_chat(&message),
                        Event::Net { name, data } => self.client.receive_net_message(&name, &data, EntityHandle::NULL),
                        Event::Asked(asked) => self.client.from_server(&asked),
                        Event::Sounds(sounds) => self.sounds.extend(sounds),
                        Event::Joined(_) => {}
                    }
                }
                // and moved on by what it asked since, as the server will move it
                let fraction = self.client.predict(self.player, &connection.unanswered());
                let view = self.client.view(self.player)?;
                between(view.previous_eye, view.eye, fraction)
            }
        };
        let view = self.client.view(self.player)?;
        let local = matches!(self.link, Link::Local(_));
        // over the network the player's angles come back late: the eyes look where the mouse
        // says now
        let angles = if local { view.angles } else { Angle::new(pitch, yaw, 0.0) };
        self.client.set_view(gmod_lua::client::View { origin: Vector::new(eye[0], eye[1], eye[2]), angles, fov: view.fov });
        self.client.think(seconds);
        // the camera takes the angles the game gave the player (a spawn, `SetEyeAngles`); in
        // this program also whatever else turned them, which comes at once
        let given = self.turned != Some(view.turned);
        self.turned = Some(view.turned);
        let apart = local && (angle_apart(view.angles.p, pitch) || angle_apart(view.angles.y, yaw));
        let turned = (given || apart).then_some((view.angles.p, view.angles.y));
        Some(Shown { eye, turned, view })
    }

    /// The models of the entities the player sees.
    pub fn drawn(&self) -> Vec<Drawn> {
        self.client.drawn(self.player)
    }

    /// What the player holds before their eyes - the weapon's view model and the hands - put
    /// where the client's Lua puts it from the eyes, and the field of view to draw it with.
    pub fn view_models(&self, eye: [f32; 3], pitch: f32, yaw: f32) -> (Vec<Drawn>, f32) {
        let (mut models, fov) = self.client.view_models(self.player);
        if !models.is_empty() {
            let (origin, angles) = self.client.view_model_view(Vector::new(eye[0], eye[1], eye[2]), Angle::new(pitch, yaw, 0.0));
            for model in &mut models {
                model.origin = origin;
                model.angles = angles;
            }
        }
        (models, fov)
    }
}

/// What the server's frame left for the host's client, handed to it.
fn deliver(client: &Runtime, host: ForHost, sounds: &mut Vec<gmod_lua::sound::SoundEvent>) {
    for message in host.chat {
        client.receive_chat(&message);
    }
    for message in host.net {
        client.receive_net_message(&message.name, &message.data, EntityHandle::NULL);
    }
    for asked in host.asked {
        client.from_server(&asked);
    }
    sounds.extend(host.sounds);
}

/// The models of the game's entities, each read from the game's files the first time an entity
/// has it, and given to the renderer.
pub struct Models {
    vfs: Arc<Vfs>,
    /// By name and skin; none for those that could not be read.
    loaded: HashMap<(String, i64), Option<ModelId>>,
    /// The map's brush models that move (`*3`), ready to upload when first drawn.
    brushes: HashMap<usize, World>,
}

impl Models {
    pub fn new(vfs: Arc<Vfs>) -> Models {
        Models { vfs, loaded: HashMap::new(), brushes: HashMap::new() }
    }

    /// With the brush models of the map that are drawn where their entities are: doors,
    /// buttons, platforms ([`gmod_world::brush_model`]).
    pub fn with_brushes(mut self, brushes: HashMap<usize, World>) -> Models {
        self.brushes = brushes;
        self
    }

    /// Where to draw the models of `drawn`. Those not seen before are read and handed to
    /// `upload`; one that cannot be read is told once and not drawn.
    pub fn place(&mut self, drawn: &[Drawn], mut upload: impl FnMut(&World) -> ModelId) -> Vec<Placed> {
        drawn
            .iter()
            .filter_map(|d| {
                let key = (d.model.to_ascii_lowercase().replace('\\', "/"), d.skin);
                let brushes = &self.brushes;
                let model = *self.loaded.entry(key).or_insert_with_key(|(name, skin)| {
                    if let Some(index) = name.strip_prefix('*').and_then(|n| n.parse::<usize>().ok()) {
                        return brushes.get(&index).filter(|w| !w.pieces.is_empty()).map(&mut upload);
                    }
                    match gmod_world::model(&self.vfs, name, usize::try_from(*skin).unwrap_or(0)) {
                        Ok(model) => Some(upload(&model)),
                        Err(e) => {
                            println!("{name}: {e}");
                            None
                        }
                    }
                });
                let (o, a) = (d.origin, d.angles);
                Some(Placed {
                    model: model?,
                    origin: [o.x, o.y, o.z],
                    angles: [a.p, a.y, a.r],
                    scale: d.scale,
                    bones: d.bones.clone(),
                    ..Placed::default()
                })
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

/// The lines typed into the terminal since the last call, read on a thread of their own
/// (started by the first call).
pub fn typed_lines() -> Vec<String> {
    static CONSOLE: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
    let console = CONSOLE.get_or_init(|| {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Mutex::new(receive)
    });
    let console = console.lock().unwrap_or_else(|e| e.into_inner());
    std::iter::from_fn(|| console.try_recv().ok()).collect()
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
        let me = gmod_net::Identity::generate().public();
        let mut game = Game::start(Arc::new(vfs), &map, "sandbox", None, 1, None, ("Player", me)).unwrap();

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

    /// The Lua of [`a_client_plays_on_a_server_over_the_network`], on both sides: the hooks,
    /// and `net` messages each way.
    const NET_INIT: &str = "
        HOOKS = {}
        hook = { Call = function(name, gm, ...) if HOOKS[name] then return HOOKS[name](...) end end }
        HOOKS.PlayerSelectSpawn = function(p) return ents.FindByClass('info_player_start')[1] end
        HOOKS.PlayerInitialSpawn = function(p)
            util.AddNetworkString('welcome') net.Start('welcome') net.WriteString('hi ' .. p:Nick()) net.Send(p)
        end
        function net.Incoming(len, from)
            local name = util.NetworkIDToString(net.ReadHeader())
            if CLIENT then GOT = name .. ': ' .. net.ReadString() else HEARD = from:Nick() .. ' ' .. from:SteamID64() .. ' ' .. name end
        end
    ";

    /// A folder of files for a test, gone when dropped.
    struct Folder(std::path::PathBuf);

    impl Folder {
        fn new(name: &str, files: &[(&str, &[u8])]) -> Folder {
            let dir = std::env::temp_dir().join(format!("opengmod-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for (path, data) in files {
                let file = dir.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, data).unwrap();
            }
            std::fs::create_dir_all(&dir).unwrap();
            Folder(dir)
        }

        fn vfs(&self) -> Vfs {
            let mut vfs = Vfs::new();
            vfs.push(Mount::new(&["GAME"], Backing::Dir(DirMount::new(&self.0))));
            vfs
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_map() -> Map {
        let floor =
            TestMap { models: vec![vec![TestBrush::cuboid([-1024.0, -1024.0, -64.0], [1024.0, 1024.0, 0.0])]], ..TestMap::default() };
        let bsp = Bsp::parse("gm_test.bsp".into(), Arc::from(floor.bytes())).unwrap();
        let mut map = Map::from_bsp("gm_test", &bsp, Arc::new(Collision::new(&bsp)));
        let start = [("classname", "info_player_start"), ("origin", "0 0 0"), ("angles", "0 90 0")];
        map.entities.push(start.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
        map
    }

    #[test]
    fn a_client_plays_on_a_server_over_the_network() {
        use std::time::{Duration, Instant};

        // the server has an addon's client Lua and a material the client does not
        let server_files = Folder::new(
            "net-server",
            &[
                ("lua/includes/init.lua", NET_INIT.as_bytes()),
                ("lua/autorun/client/hello.lua", b"HELLO = 'from the server'"),
                ("materials/test/thing.vmt", b"\"UnlitGeneric\" {}"),
                ("materials/test/thing.vtf", b"texture"),
            ],
        );
        let client_files = Folder::new("net-client", &[("lua/includes/init.lua", NET_INIT.as_bytes())]);
        let download = Folder::new("net-download", &[]);
        let addon = Folder::new(
            "net-addon",
            &[
                ("lua/autorun/client/mine.lua", b"MINE = true"),
                ("lua/autorun/client/hello.lua", b"HELLO = 'from the addon'"),
                ("materials/mine.vmt", b"\"UnlitGeneric\" {}"),
            ],
        );
        let server_vfs = Arc::new(server_files.vfs());
        let (to_test, from_server) = mpsc::channel::<String>();
        let (stop, stopped) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let mut server = Server::start(server_vfs, &test_map(), "sandbox", 4, true).unwrap();
            server
                .runtime()
                .run_string("AddCSLuaFile('autorun/client/hello.lua') resource.AddFile('materials/test/thing.vmt')", "files")
                .unwrap();
            let at = server.listen("127.0.0.1:0".parse().unwrap(), gmod_net::Identity::generate()).unwrap();
            to_test.send(at.to_string()).unwrap();
            while stopped.try_recv().is_err() {
                server.frame(0.015);
                std::thread::sleep(Duration::from_millis(5));
            }
            let heard: String = server.runtime().lua().load("return HEARD or ''").eval().unwrap();
            to_test.send(heard).unwrap();
            // the player left with the client
            for _ in 0..100 {
                server.frame(0.015);
                if server.runtime().players().is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            to_test.send(server.runtime().players().len().to_string()).unwrap();
        });
        let at: SocketAddr = from_server.recv().unwrap().parse().unwrap();

        // a server known by another key is not joined: it may be another one in its place
        let alice = gmod_net::Identity::generate();
        let lists = Folder::new("known-servers", &[]);
        let known = crate::keys::KnownServers::at(Some(lists.0.join("known.txt")));
        let wrong = crate::keys::KnownServers::at(Some(lists.0.join("wrong.txt")));
        wrong.remember(&at.to_string(), gmod_net::Identity::generate().public());
        let refused = Connection::open(at, "Alice", &alice, &wrong).err().unwrap();
        assert!(refused.contains("is not the server that was there"), "{refused}");

        // connecting: what the client lacks comes, the Lua into memory, the rest saved
        let mut connection = Connection::open(at, "Alice", &alice, &known).unwrap();
        assert_eq!(known.key(&at.to_string()), connection.key, "the server's key is kept");
        assert_eq!((connection.welcome.map.as_str(), connection.welcome.max_players), ("gm_test", 4));
        let mut vfs = client_files.vfs();
        vfs.push(Mount::new(&["GAME"], Backing::Dir(gmod_fs::DirMount::new(&download.0))));
        // an addon of the player's own, with Lua of its own and the server's file too
        vfs.insert(0, Mount::new(&["GAME"], Backing::Dir(gmod_fs::DirMount::new(&addon.0))).of_addon());
        let lua = connection.download(&vfs, Some(&download.0), &|_| {}).unwrap();
        assert!(!connection.welcome.allow_cs_lua);
        vfs.hide_in_addons(Some(super::super::client::addon_lua_hidden(&connection.listed_lua)));
        assert_eq!(lua, [("lua/autorun/client/hello.lua".to_string(), b"HELLO = 'from the server'".to_vec())]);
        assert_eq!(std::fs::read(download.0.join("materials/test/thing.vtf")).unwrap(), b"texture");
        // and the game finds it at once, though it looked in that folder before it came
        assert!(vfs.read("materials/test/thing.vtf", "GAME").is_ok_and(|data| data[..] == b"texture"[..]));
        vfs.insert(0, Mount::new(&["GAME"], Backing::Memory(gmod_fs::MemoryMount::new("server", lua))));
        let mut game = Game::join(Arc::new(vfs), &test_map(), connection, None).unwrap();
        game.client().run_string("include('autorun/client/hello.lua')", "hello").unwrap();
        assert_eq!(game.client().lua().load("return HELLO").eval::<String>().unwrap(), "from the server");
        // the addon's own Lua is not run, nor seen; its content is
        let addon_lua: bool = game
            .client()
            .lua()
            .load("return MINE == nil and not file.Exists('lua/autorun/client/mine.lua', 'GAME') and file.Exists('materials/mine.vmt', 'GAME')")
            .eval()
            .unwrap();
        assert!(addon_lua && !game.own_lua_allowed());

        // walking along y: the client sees the server move the player, and hears from it
        let forward = Keys { forward: true, ..Keys::default() };
        let started = Instant::now();
        let mut eye = [0.0; 3];
        while started.elapsed() < Duration::from_secs(10) {
            if let Some(shown) = game.frame(&forward, &Buttons::default(), 0.0, 90.0, 1.0 / 60.0) {
                eye = shown.eye;
                if eye[1] > 100.0 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        assert!(eye[1] > 100.0 && eye[0].abs() < 1.0 && (eye[2] - 64.0).abs() < 1.0, "{eye:?}");
        // standing still a while: where the client moved its player is where the server has it
        for _ in 0..40 {
            game.frame(&Keys::default(), &Buttons::default(), 0.0, 90.0, 1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(16));
        }
        let Link::Remote(connection) = &game.link else { unreachable!() };
        let on_server = connection.newest(game.player).unwrap().ent.local_pos;
        let predicted = game.client.position(game.player).unwrap();
        assert!((on_server - predicted).length() < 0.5, "{on_server:?} {predicted:?}");
        assert!(connection.unanswered().len() < 10);
        assert_eq!(game.client().lua().load("return GOT").eval::<String>().unwrap(), "welcome: hi Alice");
        assert!(game.multiplayer() && game.closed().is_none());

        // a message the other way
        game.client().run_string("util.AddNetworkString('thanks') net.Start('thanks') net.SendToServer()", "thanks").unwrap();
        for _ in 0..20 {
            game.frame(&Keys::default(), &Buttons::default(), 0.0, 90.0, 1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(10));
        }
        game.leave();
        stop.send(()).unwrap();
        assert_eq!(from_server.recv().unwrap(), format!("Alice {} thanks", crate::keys::steam_id64(&alice.public())), "known by her key");
        assert_eq!(from_server.recv().unwrap(), "0", "the player left with the client");
        server.join().unwrap();
    }

    #[test]
    fn the_menu_finds_servers_and_their_players() {
        use std::time::{Duration, Instant};
        let server_files = Folder::new(
            "browse-server",
            &[("lua/includes/init.lua", NET_INIT.as_bytes()), ("gamemodes/sandbox/sandbox.txt", b"\"sandbox\" { \"title\" \"Sandbox\" }")],
        );
        let mut server = Server::start(Arc::new(server_files.vfs()), &test_map(), "sandbox", 4, true).unwrap();
        server.name = "Alice's game".into();
        let at = server.listen("127.0.0.1:0".parse().unwrap(), gmod_net::Identity::generate()).unwrap();
        server.runtime().join(gmod_lua::PlayerInfo { name: "Bob".into(), bot: true, ..Default::default() }).unwrap();

        let saved = Folder::new("browse-lists", &[]);
        std::fs::write(saved.0.join("favorites.txt"), format!("{at}\n127.0.0.1:1\n")).unwrap();
        let mut browser = crate::browser::Browser::new(Some(saved.0.clone()));
        let menu = Runtime::new(Realm::Menu, Arc::new(Vfs::new())).unwrap();
        menu.set_favorite_servers(browser.favorites());
        menu.run_string(
            &format!(
                "serverlist.PingServer('{at}', function(ping, name, desc, map, players, max, bots)
                     PING = table.concat({{ name, desc, map, players, max, bots }}, '|')
                 end)
                 serverlist.PingServer('127.0.0.1:1', function(ping, name) DEAD = name or 'no answer' end)
                 serverlist.PlayerList('{at}', function(t) PLAYERS = t[1] and t[1].name or 'none' end)
                 FAV = {{}}
                 serverlist.Query({{ Type = 'favorite',
                     Callback = function(ping, name) table.insert(FAV, name) return true end,
                     CallbackFailed = function(address) table.insert(FAV, 'failed ' .. address) return true end,
                     Finished = function() FAV_DONE = true end }})
                 serverlist.Query({{ Type = 'internet', Finished = function() INTERNET_DONE = true end }})"
            ),
            "browse",
        )
        .unwrap();
        let started = Instant::now();
        let done = || menu.lua().load("return FAV_DONE == true and PING ~= nil and DEAD ~= nil and PLAYERS ~= nil").eval::<bool>().unwrap();
        while !done() && started.elapsed() < Duration::from_secs(5) {
            server.frame(0.01);
            browser.poll(&menu);
            std::thread::sleep(Duration::from_millis(5));
        }
        let got: String = menu
            .lua()
            .load("return table.concat({ PING, DEAD, PLAYERS, table.concat(FAV, ','), tostring(INTERNET_DONE) }, '\\n')")
            .eval()
            .unwrap();
        assert_eq!(got, "Alice's game|Sandbox|gm_test|0|4|1\nno answer\nBob\nAlice's game,failed 127.0.0.1:1\ntrue");
        // the servers played on, newest first
        browser.played_on("10.0.0.1:27015");
        browser.played_on(&at.to_string());
        browser.played_on("10.0.0.1:27015");
        assert_eq!(std::fs::read_to_string(saved.0.join("history.txt")).unwrap(), format!("10.0.0.1:27015\n{at}\n"));
        // favourites the menu changed are kept
        menu.run_string("serverlist.RemoveServerFromFavorites('127.0.0.1:1')", "unfavorite").unwrap();
        browser.poll(&menu);
        assert_eq!(browser.favorites(), [at.to_string()]);
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
            bones: Vec::new(),
        };
        // not in the game's files: not drawn, and not looked for again
        assert!(models.place(&[drawn.clone(), drawn.clone()], |_| unreachable!("nothing to upload")).is_empty());
        assert_eq!(models.loaded.len(), 1);
        assert_eq!(models.loaded.get(&(drawn.model.clone(), 0)), Some(&None));
    }

    #[test]
    fn keys_make_a_command() {
        let keys = Keys { forward: true, left: true, run: true, ..Keys::default() };
        let buttons = Buttons { attack: true, using: true, wheel: -1, mouse: [3, -4], ..Buttons::default() };
        let cmd = command(&keys, &buttons, 10.0, 90.0);
        assert_eq!(cmd.view_angles, Angle::new(10.0, 90.0, 0.0));
        assert_eq!((cmd.mouse_wheel, cmd.mouse_x, cmd.mouse_y), (-1, 3, -4));
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
            holding: false,
            flashlight: false,
            turned: 0,
        };
        assert_eq!(status(&view), "health 80 - weapon_pistol 12/36");
        view.weapon = None;
        view.armor = 15;
        assert_eq!(status(&view), "health 80, armor 15");
        view.alive = false;
        assert_eq!(status(&view), "dead - click to spawn again");
    }
}
