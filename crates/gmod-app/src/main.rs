//! `opengmod`: Garry's Mod's main menu ([`menu`], drawn by Chromium), and the maps it starts on
//! the screen - the world, brush entities and static props with their textures and light, and
//! the sky box - with the game running in them: the Lua of the server, the gamemode and the
//! addons ([`game`]), and the player walking through it as the game moves them - alone, as
//! the host of a game others join over the network, or on another computer's server
//! (`--connect`, [`client`]). A dedicated server (`--dedicated`) runs a game without a window
//! for those who join ([`server`]). Drawn with Direct3D 12 on Windows (Vulkan when that is
//! missing). Without the game's Lua (`--no-lua`, or a map file without the game) the player
//! only walks ([`gmod_movement`]).
//!
//! To play, the player signs in through Steam as Garry's Mod ([`gmod_steam`]): Steam must run,
//! with an account that owns the game. Their name is their name on Steam, the workshop items
//! mounted those they subscribed to.

// a program of windows, as the game is: no terminal of its own ([`terminal`])
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
#![deny(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use gmod_bsp::Collision;
use gmod_fs::UserDir;
use gmod_fs::Vfs;
use gmod_fs::install::Install;
use gmod_fs::mount::{MountOptions, mount_install};
use gmod_movement::{Player, Settings};
use gmod_render::block_on;
use gmod_world::World;
use gmod_world::gpu::Camera;

#[macro_use]
mod console;
mod audio;
mod browser;
mod client;
mod effects;
mod game;
mod icons;
mod keys;
mod menu;
mod proto;
mod server;
mod settings;
mod terminal;
mod walk;

use game::{Buttons, Game, Models};
use gmod_lua::weaponselect::Choose;
use walk::{Keys, Walker};

const USAGE: &str = "\
opengmod - Garry's Mod's menu and maps from your installation, with the game's Lua and your addons

USAGE:
    opengmod [OPTIONS] [MAP]
    opengmod --connect <ADDRESS> [OPTIONS]
    opengmod --dedicated [OPTIONS] MAP
    opengmod --master [--port <PORT>]

    MAP                  a map to start in: one of the game (gm_construct), or a .bsp file;
                         without one, the main menu

OPTIONS:
    --gmod <DIR>         the Garry's Mod folder (default: GMOD_ROOT, then Steam's libraries)
    --no-addons          the game only: no workshop items, no addon files
    --no-lua             no Lua: only walk through the map
    --no-html            no Chromium: no main menu (gm_construct starts) and no HTML panels
    --gamemode <NAME>    the gamemode of a game started here (default: sandbox)
    --maxplayers <N>     players a game started here takes; more than 1: others join it over
                         the network (a listen server)
    --port <PORT>        the UDP port a server here listens on (default: 27015)
    --connect <ADDRESS>  plays on a server: host or host:port
    --dedicated          a server without a window or a player of its own (16 players
                         unless --maxplayers says otherwise); lines typed are its console,
                         status lists the players, lua_run runs Lua, quit
                         stops it
    --hostname <NAME>    the server's name its players see
    --announce <ADDRESS> a master server this server tells it is here, for the browsers'
                         internet list (also those in ~/.opengmod/servers/masters.txt;
                         none with sv_lan 1)
    --master             a master server: the list of the servers that tell it they are
                         here, for the browsers of those who name it in their masters.txt
                         (UDP port 27011 unless --port says otherwise)
    --tickrate <N>       ticks a second of a game started here (also -tickrate); GMod's 66
                         by default. Joined, a server's are taken
    -h, --help           this text

    Playing takes Steam running, signed in to an account that owns Garry's Mod: the player's
    name is theirs on Steam. A dedicated server and a master server need no Steam.

CONTROLS:
    in a game, Esc shows the main menu and takes you back; click the window to look around
    W A S D to walk, Shift to run, Alt to walk slowly, Space to jump, Ctrl to duck
    left and right mouse buttons to fire, R to reload, E to use, F for the flashlight
    1 to 6 and the mouse wheel to choose a weapon, Z to undo
    the physics gun: hold the left button to carry, the wheel to take it farther or
    nearer, E and the mouse to turn it, the right button to freeze it, R to unfreeze
    V to fly through walls (noclip) and back
    hold Q for the spawn menu, C for the context menu: the mouse works in them
    F11 full screen, F12 a screenshot (saved in ~/.opengmod/screenshots)

    openGMOD opens no terminal of its own: the console is in the game (`), and what it says
    goes there. A dedicated server's console is the terminal it runs in.

    Lines typed into the console are the game's: give weapon_crowbar,
    gm_spawn models/props_c17/oildrum001.mdl, the commands of the addons; connect <address>
    joins a server, disconnect leaves it, lua_run_cl runs Lua in your client and lua_run_menu
    in the main menu.";

/// Units a second flying; Shift makes it four times as much.
const SPEED: f32 = 400.0;
/// The game's field of view, across a 4:3 picture.
const FOV: f32 = 75.0;

/// The map started without the main menu.
const DEFAULT_MAP: &str = "gm_construct";

struct Args {
    /// A map to start in; none: the main menu.
    map: Option<String>,
    gmod: Option<PathBuf>,
    addons: bool,
    lua: bool,
    html: bool,
    gamemode: Option<String>,
    max_players: Option<u32>,
    port: Option<u16>,
    connect: Option<String>,
    dedicated: bool,
    hostname: Option<String>,
    /// Master servers a server here tells it is here.
    announce: Vec<String>,
    master: bool,
    /// Ticks a second (`-tickrate`); none: GMod's 66.
    tick_rate: Option<f64>,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args {
        map: None,
        gmod: None,
        addons: true,
        lua: true,
        html: true,
        gamemode: None,
        max_players: None,
        port: None,
        connect: None,
        dedicated: false,
        hostname: None,
        announce: Vec::new(),
        master: false,
        tick_rate: None,
    };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--gmod" => args.gmod = Some(raw.next().ok_or("--gmod needs a folder")?.into()),
            "--map" => args.map = Some(raw.next().ok_or("--map needs a map")?),
            "--no-addons" => args.addons = false,
            "--no-lua" => args.lua = false,
            "--no-html" => args.html = false,
            "--gamemode" => args.gamemode = Some(raw.next().ok_or("--gamemode needs a gamemode")?),
            "--maxplayers" => {
                let n = raw.next().and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=gmod_lua::player::MAX_PLAYERS).contains(n));
                args.max_players = Some(n.ok_or(format!("--maxplayers needs a number from 1 to {}", gmod_lua::player::MAX_PLAYERS))?);
            }
            "--port" => args.port = Some(raw.next().and_then(|p| p.parse().ok()).ok_or("--port needs a port")?),
            "--connect" => args.connect = Some(raw.next().ok_or("--connect needs an address")?),
            "--dedicated" => args.dedicated = true,
            "--hostname" => args.hostname = Some(raw.next().ok_or("--hostname needs a name")?),
            "--announce" => args.announce.push(raw.next().ok_or("--announce needs a master server's address")?),
            "--master" => args.master = true,
            "--tickrate" | "-tickrate" => {
                let rate = raw.next().and_then(|n| n.parse::<f64>().ok()).filter(|n| (10.0..=1000.0).contains(n));
                args.tick_rate = Some(rate.ok_or("--tickrate needs a number of ticks a second, 10 to 1000")?);
            }
            "-h" | "--help" => return Ok(None),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if args.map.is_none() => args.map = Some(arg),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    Ok(Some(args))
}

/// How games are started: from where, with what.
#[derive(Clone)]
struct Setup {
    gmod: Option<PathBuf>,
    addons: bool,
    lua: bool,
    gamemode: String,
    /// Addons switched off in the menu.
    disabled: HashSet<String>,
    /// Players a game started here takes: more than one, it listens for them on `port`.
    max_players: u32,
    port: u16,
    /// Master servers a server here tells it is here, besides those of `servers/masters.txt`.
    announce: Vec<String>,
    /// The player's name, and their key: who they are on every server.
    name: String,
    identity: gmod_net::Identity,
    /// The workshop items the player subscribed to, as Steam lists them; none without Steam
    /// (a dedicated server), which mounts every item of the workshop folder.
    subscribed: Option<HashSet<u64>>,
}

impl Setup {
    /// How Garry's Mod's files are mounted: the addons switched off left out.
    fn mount_options(&self) -> MountOptions {
        let unpack_dir = UserDir::default_root().map(|root| root.join("cache").join("workshop"));
        MountOptions {
            addons: self.addons,
            unpack_dir,
            disabled: self.disabled.clone(),
            subscribed: self.subscribed.clone(),
            ..MountOptions::default()
        }
    }

    /// The game's files as Garry's Mod mounts them, the addons switched off left out.
    fn mount(&self, install: &Install) -> Vfs {
        mount_install(install, &self.mount_options()).0
    }
}

/// A map read, with what it needs from the game.
struct Loaded {
    world: World,
    collision: Arc<Collision>,
    /// The map's light for the models of entities.
    light: gmod_world::light::MapLight,
    /// The game's files and the map as the Lua of the server takes it, unless there is no Lua
    /// to run.
    lua: Option<(Arc<Vfs>, gmod_lua::Map)>,
    camera: Camera,
    /// Where players start: the feet, and the angles.
    spawn: Option<([f32; 3], [f32; 3])>,
    name: String,
    /// The brush models of the entities that may move, drawn where those are.
    brushes: HashMap<usize, World>,
}

/// What may be loaded in the background: a map of the player's own, or a server to join.
enum Loadable {
    Map(String),
    Server(String),
}

/// What a loading in the background made ready.
enum Prepared {
    Map(Box<Loaded>),
    Server(Box<Joining>),
}

/// A server's game made ready: the connection open, the files fetched, its map loaded.
struct Joining {
    loaded: Loaded,
    connection: client::Connection,
    addr: SocketAddr,
}

/// What the server being joined says of itself, for the loading screen (`GameDetails`), and the
/// files it has the client fetch.
struct Details {
    server: String,
    loading_url: String,
    map: String,
    max_players: u32,
    gamemode: String,
    downloads: Vec<String>,
}

/// A loading going on in the background ([`App::begin_loading`]).
struct Loading {
    done: std::sync::mpsc::Receiver<Result<Prepared, String>>,
    /// What it does now.
    status: Arc<std::sync::Mutex<String>>,
    /// What the server said, until the loading screen took it.
    details: Arc<std::sync::Mutex<Option<Details>>>,
    /// The map or the server: what the player is told failed.
    name: String,
}

/// Gets a server's game ready: connects, fetches the files it says to have, loads its map;
/// `say` hears what it does.
fn prepare_join(setup: &Setup, address: &str, say: &dyn Fn(&str)) -> Result<Joining, String> {
    prepare_join_telling(setup, address, say, &Arc::new(std::sync::Mutex::new(None)))
}

/// [`prepare_join`], telling `details` what the server is and has the client fetch.
fn prepare_join_telling(
    setup: &Setup,
    address: &str,
    say: &dyn Fn(&str),
    details: &Arc<std::sync::Mutex<Option<Details>>>,
) -> Result<Joining, String> {
    let addr = server_address(address)?;
    say(&format!("Connecting to {addr}"));
    let mut connection = client::Connection::open(addr, &setup.name, &setup.identity, &keys::KnownServers::open())?;
    let welcome = connection.welcome.clone();
    say!("{}: {} ({}, {} players)", welcome.server, welcome.map, welcome.gamemode, welcome.max_players);
    let install = Install::locate(setup.gmod.as_deref()).map_err(|e| e.to_string())?;
    let mut vfs = setup.mount(&install);
    // what servers had downloaded is kept, behind the game's own files
    let download = UserDir::default_root().map(|root| root.join("download"));
    if let Some(dir) = &download {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        vfs.push(gmod_fs::Mount::new(&["GAME"], gmod_fs::Backing::Dir(gmod_fs::DirMount::new(dir))));
    }
    let told = |fetching: client::Fetching| match fetching {
        client::Fetching::Listed(files) => {
            if let Ok(mut details) = details.lock() {
                *details = Some(Details {
                    server: welcome.server.clone(),
                    loading_url: welcome.loading_url.clone(),
                    map: welcome.map.clone(),
                    max_players: welcome.max_players,
                    gamemode: welcome.gamemode.clone(),
                    downloads: files.to_vec(),
                });
            }
        }
        client::Fetching::Coming(file) => say(&format!("Downloading {file}")),
    };
    let lua = connection.download(&vfs, download.as_deref(), &told)?;
    // the player's addons run on the server's word: their Lua the server lacks does not
    if !welcome.allow_cs_lua {
        vfs.hide_in_addons(Some(client::addon_lua_hidden(&connection.listed_lua)));
    }
    // the server's client Lua before the client's own
    if !lua.is_empty() {
        vfs.insert(0, gmod_fs::Mount::new(&["GAME"], gmod_fs::Backing::Memory(gmod_fs::MemoryMount::new("server", lua))));
    }
    say(&format!("Loading {}", welcome.map));
    let loaded = load_from(vfs, &welcome.map, true)?;
    Ok(Joining { loaded, connection, addr })
}

/// Reads the map and everything it needs from the game.
fn load(setup: &Setup, map: &str) -> Result<Loaded, String> {
    let on_disk = Path::new(map).is_file();
    let (vfs, game) = match Install::locate(setup.gmod.as_deref()) {
        Ok(install) => (setup.mount(&install), true),
        // a map file can be shown without the game, only not with its materials
        Err(e) if on_disk => {
            say!("{e}\nThe map is drawn without the game's materials.");
            (gmod_fs::Vfs::new(), false)
        }
        Err(e) => return Err(e.to_string()),
    };
    load_from(vfs, map, setup.lua && game)
}

/// Reads a map - a file, or one of the game's - and mounts its pakfile: the map, and where it
/// was.
fn read_map(vfs: &mut Vfs, map: &str) -> Result<(gmod_bsp::Bsp, String), String> {
    let on_disk = Path::new(map).is_file();
    let path = if on_disk {
        map.to_string()
    } else {
        let name = map.trim_end_matches(".bsp");
        if name.contains(['/', '\\']) { format!("{name}.bsp") } else { format!("maps/{name}.bsp") }
    };
    let bytes = if on_disk { std::fs::read(&path) } else { vfs.read(&path, "GAME") }.map_err(|e| format!("{path}: {e}"))?;
    let bsp = gmod_bsp::Bsp::parse(PathBuf::from(&path), Arc::from(bytes)).map_err(|e| e.to_string())?;
    gmod_world::mount_pakfile(vfs, &bsp).map_err(|e| format!("{path}: pakfile: {e}"))?;
    Ok((bsp, path))
}

/// Reads a map from these files with everything it needs; with the game's Lua when `lua`.
fn load_from(mut vfs: Vfs, map: &str, lua: bool) -> Result<Loaded, String> {
    let started = Instant::now();
    let (bsp, path) = read_map(&mut vfs, map)?;
    let world = gmod_world::load(&vfs, &bsp);
    let (collision, unread_physics) = gmod_world::collision(&vfs, &bsp);
    say!("{path}: {} triangles, {} textures, in {:.1} s", world.triangles(), world.textures.len(), started.elapsed().as_secs_f64());
    let lists = [
        ("Missing materials", &world.missing_materials),
        ("Missing textures", &world.missing_textures),
        ("Missing models", &world.missing_models),
        ("Physics models that could not be read, their props not solid", &unread_physics),
    ];
    for (what, names) in lists {
        if !names.is_empty() {
            let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
            let more = if names.len() > 3 { format!(" and {} more", names.len() - 3) } else { String::new() };
            say!("{what} ({}): {}{more}", names.len(), shown.join(", "));
        }
    }
    let (eye, target) = gmod_world::default_view(&bsp);
    let name = Path::new(&path).file_stem().map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned());
    let collision = Arc::new(collision);
    // the brush models of what may move, drawn where their entities are
    let brushes = bsp
        .entities
        .iter()
        .filter(|e| e.moves())
        .filter_map(|e| e.brush_model())
        .map(|m| (m, gmod_world::brush_model(&vfs, &bsp, m)))
        .collect();
    let lua = lua.then(|| {
        let map = gmod_lua::Map::from_bsp(&name, &bsp, collision.clone());
        (Arc::new(vfs), map)
    });
    let light = gmod_world::light::MapLight::new(&bsp);
    Ok(Loaded {
        world,
        collision,
        light,
        lua,
        camera: Camera::looking_at(eye, target, FOV),
        spawn: gmod_world::spawn_point(&bsp),
        name,
        brushes,
    })
}

/// A dedicated server: the game on a map for those who join over the network, without a
/// window; lines typed are its console. Runs until `quit`.
/// A master server: lists the servers that tell it they are here, for the browsers that ask.
/// Lines typed are its console: `status` lists the servers, `quit` stops it.
fn master(port: u16) -> Result<(), String> {
    let mut master = gmod_net::Master::bind(SocketAddr::from(([0, 0, 0, 0], port))).map_err(|e| format!("port {port}: {e}"))?;
    say!("openGMOD master server on UDP port {port}; type status or quit");
    loop {
        master.poll(Instant::now());
        for line in game::typed_lines() {
            match line.trim() {
                "quit" | "exit" => return Ok(()),
                "status" => {
                    let servers = master.servers();
                    say!("{} servers", servers.len());
                    for server in servers {
                        say!("  {server}");
                    }
                }
                "" => {}
                other => say!("{other}: status or quit"),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn dedicated(setup: &Setup, map: &str, hostname: &str) -> Result<(), String> {
    let install = Install::locate(setup.gmod.as_deref()).map_err(|e| e.to_string())?;
    say!("Garry's Mod: {}", install.root.display());
    let mut vfs = setup.mount(&install);
    let (bsp, path) = read_map(&mut vfs, map)?;
    let (collision, unread_physics) = gmod_world::collision(&vfs, &bsp);
    if !unread_physics.is_empty() {
        say!("Physics models that could not be read, their props not solid: {}", unread_physics.len());
    }
    let name = Path::new(&path).file_stem().map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned());
    let lua_map = gmod_lua::Map::from_bsp(&name, &bsp, Arc::new(collision));
    let started = Instant::now();
    let mut server = server::Server::start(Arc::new(vfs), &lua_map, &setup.gamemode, setup.max_players, true)?;
    server.name = hostname.to_string();
    let identity = keys::server_identity();
    let key = identity.public();
    let at = server.listen(SocketAddr::from(([0, 0, 0, 0], setup.port)), identity).map_err(|e| format!("port {}: {e}", setup.port))?;
    server.set_masters(browser::masters(&setup.announce));
    say!(
        "{hostname}: {name} ({}) for {} players, started in {:.1} s, on UDP port {}; type status, quit or the server's commands",
        setup.gamemode,
        setup.max_players,
        started.elapsed().as_secs_f64(),
        at.port()
    );
    say!("Its key, which players know it by: {key}");
    let tick = Duration::from_secs_f64(gmod_lua::timer::tick_interval());
    let mut next = Instant::now();
    loop {
        server.frame(gmod_lua::timer::tick_interval());
        for line in game::typed_lines() {
            match line.trim() {
                "quit" | "exit" => {
                    server.shut_down("the server is shutting down");
                    return Ok(());
                }
                "status" => {
                    let players = server.remote_players();
                    say!("{} of {} players", players.len(), setup.max_players);
                    for (name, steam_id, rtt) in players {
                        say!("  {name} ({steam_id}): {} ms", rtt.map_or(0, |r| r.as_millis()));
                    }
                }
                text => match text.strip_prefix("lua_run ") {
                    Some(code) => {
                        // errors are reported as they happen
                        let _ = server.runtime().run_string(code, "lua_run");
                    }
                    None => server.runtime().run_command(&line),
                },
            }
        }
        // a tick a tick; one that ran late is not made up for long after
        next += tick;
        let now = Instant::now();
        match next.checked_duration_since(now) {
            Some(wait) => std::thread::sleep(wait),
            None if now - next > Duration::from_secs(1) => next = now,
            None => {}
        }
    }
}

/// An address to connect to: `host` or `host:port`.
fn server_address(address: &str) -> Result<SocketAddr, String> {
    if let Ok(ip) = address.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, proto::DEFAULT_PORT));
    }
    let with_port = if address.parse::<SocketAddr>().is_ok() || address.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        address.to_string()
    } else {
        format!("{address}:{}", proto::DEFAULT_PORT)
    };
    with_port.to_socket_addrs().map_err(|e| format!("{address}: {e}"))?.next().ok_or_else(|| format!("{address}: no such host"))
}

/// How much a graphics adapter is wanted, lowest first: a real GPU before a software one (WARP,
/// llvmpipe), a discrete one before an integrated one, and of the same kind Direct3D 12 before
/// Vulkan.
fn adapter_rank(kind: wgpu::DeviceType, backend: wgpu::Backend, wanted: Option<wgpu::Backend>) -> (bool, u8, bool) {
    let kind = match kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::Other => 2,
        wgpu::DeviceType::VirtualGpu => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    (wanted.is_some_and(|w| w != backend), kind, backend != wgpu::Backend::Dx12)
}

/// The graphics APIs the player may choose (`opengmod_renderer`), as the setting names them.
const RENDERERS: [(&str, wgpu::Backend); 2] = [("dx12", wgpu::Backend::Dx12), ("vulkan", wgpu::Backend::Vulkan)];

fn renderer_name(backend: wgpu::Backend) -> &'static str {
    RENDERERS.iter().find(|(_, b)| *b == backend).map_or("other", |(name, _)| name)
}

/// The window and what draws into it.
struct Gpu {
    window: Arc<Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The surface's format with sRGB encoding: shaders give linear colour.
    view_format: wgpu::TextureFormat,
    /// The map's; none while there is no map.
    renderer: Option<gmod_render::Renderer>,
    /// When the map came up: materials that change with time count from then.
    map_started: Instant,
    /// The client's 2D over the world: the HUD, the menus.
    overlay: gmod_render::Overlay,
    /// The main menu's, over everything.
    menu_overlay: gmod_render::Overlay,
    /// The graphics API it draws with, the GPU's name, and the APIs the GPU could draw this
    /// window with.
    backend: wgpu::Backend,
    adapter_name: String,
    backends: Vec<wgpu::Backend>,
}

impl Gpu {
    /// A window, drawn into with the graphics API `renderer` names (`opengmod_renderer`) when
    /// the GPU has it, else the best it has.
    fn new(
        event_loop: &ActiveEventLoop,
        title: &str,
        vsync: bool,
        fullscreen: bool,
        size: (u32, u32),
        renderer: &str,
    ) -> Result<Gpu, String> {
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::PhysicalSize::new(size.0.max(320), size.1.max(240)))
            .with_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("window: {e}"))?);
        let mut descriptor = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle()));
        let backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
        descriptor.backends = backends;
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance.create_surface(window.clone()).map_err(|e| format!("surface: {e}"))?;
        // chosen here and not by request_adapter, which takes Vulkan first when both drive a GPU
        let wanted = RENDERERS.iter().find(|(name, _)| renderer.eq_ignore_ascii_case(name)).map(|&(_, b)| b);
        let adapters: Vec<wgpu::Adapter> =
            block_on(instance.enumerate_adapters(backends)).into_iter().filter(|adapter| adapter.is_surface_supported(&surface)).collect();
        let mut available: Vec<wgpu::Backend> = adapters.iter().map(|a| a.get_info().backend).collect();
        available.sort_by_key(|&b| renderer_name(b));
        available.dedup();
        let adapter = adapters
            .into_iter()
            .min_by_key(|adapter| {
                let info = adapter.get_info();
                adapter_rank(info.device_type, info.backend, wanted)
            })
            .ok_or("no graphics adapter with Direct3D 12 or Vulkan")?;
        let info = adapter.get_info();
        say!("GPU: {} ({:?})", info.name, info.backend);
        let features = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("opengmod"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .map_err(|e| format!("graphics device: {e}"))?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = *capabilities.formats.first().ok_or("the window cannot be drawn into")?;
        let view_format = format.add_srgb_suffix();
        let size = window.inner_size();
        // what is seen through (glass, water) sees the frame drawn so far: copied, where the
        // window lets that be
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | (capabilities.usages & wgpu::TextureUsages::COPY_SRC);
        let config = wgpu::SurfaceConfiguration {
            usage,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync },
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![view_format],
        };
        surface.configure(&device, &config);
        let overlay = gmod_render::Overlay::new(&device, &queue, view_format);
        let menu_overlay = gmod_render::Overlay::new(&device, &queue, view_format);
        Ok(Gpu {
            window,
            instance,
            surface,
            device,
            queue,
            config,
            view_format,
            renderer: None,
            map_started: Instant::now(),
            overlay,
            menu_overlay,
            backend: info.backend,
            adapter_name: info.name.clone(),
            backends: available,
        })
    }

    /// The map to draw from now on, or none.
    fn set_world(&mut self, world: Option<&World>) {
        self.renderer = None;
        // the client's images were of the game left
        self.overlay = gmod_render::Overlay::new(&self.device, &self.queue, self.view_format);
        let Some(world) = world else { return };
        let started = Instant::now();
        let renderer = gmod_render::Renderer::new(&self.device, &self.queue, self.view_format, world);
        say!("Uploaded to the GPU in {:.1} s", started.elapsed().as_secs_f64());
        if !renderer.failed_textures.is_empty() {
            say!("Textures that could not be shown: {}", renderer.failed_textures.len());
        }
        self.renderer = Some(renderer);
        self.map_started = Instant::now();
    }

    /// Waits for the screen to show each frame, or not (`mat_vsync`).
    fn set_vsync(&mut self, vsync: bool) {
        self.config.present_mode = if vsync { wgpu::PresentMode::AutoVsync } else { wgpu::PresentMode::AutoNoVsync };
        self.surface.configure(&self.device, &self.config);
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// The map, or black where there is none.
    fn draw_3d(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        texture: &wgpu::Texture,
        size: (u32, u32),
        camera: &Camera,
    ) {
        match &mut self.renderer {
            Some(renderer) => {
                renderer.set_time(self.map_started.elapsed().as_secs_f32());
                renderer.render(&self.device, &self.queue, encoder, view, texture, size, camera);
            }
            None => {
                encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("no map"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
        }
    }

    /// The 2D of the client and of the menu over the map.
    fn draw_2d(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        size: (u32, u32),
        client: Option<&gmod_lua::Runtime>,
        menu: Option<&gmod_lua::Runtime>,
    ) {
        if let Some(client) = client {
            let changed = draw_2d(&self.device, &self.queue, &mut self.overlay, encoder, view, size, client);
            // the pages of glyphs that text drawn in the world is cut from: the renderer's too,
            // when they change and the first time
            if let Some(renderer) = &mut self.renderer {
                client.with_canvas(|_, fonts| {
                    for (page, glyphs) in fonts.pages().iter().enumerate() {
                        let name = format!("{}{page}", gmod_lua::render3d::GLYPHS);
                        if changed.contains(&(page as u32)) || !renderer.has_texture(&name) {
                            let side = gmod_ui::font::PAGE_SIZE;
                            renderer.set_effect_pixels(&self.device, &self.queue, &name, side, side, glyphs.pixels.clone());
                        }
                    }
                });
            }
        }
        if let Some(menu) = menu {
            draw_2d(&self.device, &self.queue, &mut self.menu_overlay, encoder, view, size, menu);
        }
    }

    /// Draws one more frame into a texture of its own and saves it as a PNG in
    /// `~/.opengmod/screenshots`.
    fn screenshot(
        &mut self,
        camera: &Camera,
        client: Option<&gmod_lua::Runtime>,
        menu: Option<&gmod_lua::Runtime>,
        name: &str,
    ) -> Result<PathBuf, String> {
        let bgra = matches!(self.view_format, wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm);
        let rgba = matches!(self.view_format, wgpu::TextureFormat::Rgba8UnormSrgb | wgpu::TextureFormat::Rgba8Unorm);
        if !bgra && !rgba {
            return Err(format!("screenshots of {:?} pictures are not made yet", self.view_format));
        }
        let (width, height) = (self.config.width, self.config.height);
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.view_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("screenshot") });
        self.draw_3d(&mut encoder, &view, &texture, (width, height), camera);
        self.draw_2d(&mut encoder, &view, (width, height), client, menu);
        // copies out of a texture go in rows of a multiple of 256 bytes
        let row = (width * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) },
            },
            size,
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |_| ());
        self.device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("the GPU: {e}"))?;
        let data = buffer.slice(..).get_mapped_range().map_err(|e| format!("the picture: {e}"))?.to_vec();
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let p = &data[y * row as usize + x * 4..][..4];
                rgb.extend(if bgra { [p[2], p[1], p[0]] } else { [p[0], p[1], p[2]] });
            }
        }
        let dir = UserDir::default_root().ok_or("there is no home folder for the screenshots")?.join("screenshots");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let file = dir.join(format!("{name}-{seconds}.png"));
        std::fs::write(&file, gmod_material::png::encode(width, height, 3, &rgb)).map_err(|e| format!("{}: {e}", file.display()))?;
        Ok(file)
    }

    /// A model drawn alone, `size` pixels square, as `camera` sees it (a spawn icon): its RGBA
    /// pixels, transparent where it is not.
    fn picture_model(&self, model: gmod_render::ModelId, camera: &Camera, size: u32) -> Option<gmod_material::png::Rgba> {
        let renderer = self.renderer.as_ref()?;
        let bgra = matches!(self.view_format, wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Bgra8Unorm);
        let extent = wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("model picture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.view_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("model picture") });
        renderer.render_model(&self.device, &self.queue, &mut encoder, &view, (size, size), camera, model);
        let row = (size * 4).div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("model picture"),
            size: u64::from(row) * u64::from(size),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(size) },
            },
            extent,
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |_| ());
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        let data = buffer.slice(..).get_mapped_range().ok()?.to_vec();
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size as usize {
            for x in 0..size as usize {
                let p = &data[y * row as usize + x * 4..][..4];
                pixels.extend(if bgra { [p[2], p[1], p[0], p[3]] } else { [p[0], p[1], p[2], p[3]] });
            }
        }
        Some(gmod_material::png::Rgba { width: size, height: size, pixels })
    }

    fn draw(&mut self, camera: &Camera, client: Option<&gmod_lua::Runtime>, menu: Option<&gmod_lua::Runtime>) -> bool {
        // a frame that no longer fits the window as well as it could is still shown, and the
        // surface made again after it: dropping it each time would leave the window empty
        let mut suboptimal = false;
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                suboptimal = true;
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return false;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
                    self.surface = surface;
                    self.surface.configure(&self.device, &self.config);
                }
                return false;
            }
            // hidden, or not ready yet: the next frame
            _ => return false,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(self.view_format), ..Default::default() });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        let size = (self.config.width, self.config.height);
        self.draw_3d(&mut encoder, &view, &frame.texture, size, camera);
        self.draw_2d(&mut encoder, &view, size, client, menu);
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
        if suboptimal {
            let size = self.window.inner_size();
            self.resize(size.width, size.height);
            self.surface.configure(&self.device, &self.config);
        }
        true
    }
}

/// Draws what a realm painted in 2D: the pages' new pictures, the atlas's new glyphs and the
/// new images go to the GPU first. The pages of glyphs that changed.
fn draw_2d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlay: &mut gmod_render::Overlay,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    size: (u32, u32),
    runtime: &gmod_lua::Runtime,
) -> Vec<u32> {
    for (id, picture) in runtime.take_page_pictures() {
        overlay.update_image(device, queue, id, picture.width, picture.height, &picture.pixels);
    }
    let (changed, missing) =
        runtime.with_canvas(|canvas, fonts| (overlay.update_glyphs(device, queue, fonts), overlay.missing_images(canvas)));
    for id in missing {
        let pixels = runtime.image(id).and_then(|(_, pixels)| pixels);
        overlay.set_image(device, queue, id, pixels.as_ref().map(|p| (p.width, p.height, p.pixels.as_slice())));
    }
    runtime.with_canvas(|canvas, _| overlay.render(device, queue, encoder, view, size, canvas));
    changed
}

struct App {
    /// The map to give the GPU when it is there.
    world: Option<World>,
    /// The map's solid world; none while there is no map.
    collision: Option<Arc<Collision>>,
    /// The game running in the map; without it the player only walks.
    game: Option<Game>,
    /// The models of the game's entities.
    models: Option<Models>,
    /// The map's light for them, and what it was at places already asked about.
    light: Option<gmod_world::light::MapLight>,
    lit: HashMap<[i32; 3], gmod_world::light::ModelLight>,
    /// The game's sound.
    audio: Option<audio::Audio>,
    /// The game's status for the title: health, the weapon.
    status: String,
    settings: Settings,
    /// Where players start: the feet, and the angles.
    spawn: Option<([f32; 3], [f32; 3])>,
    /// The player, walking; `None` while flying through walls.
    walker: Option<Walker>,
    name: String,
    camera: Camera,
    /// Where the game's view looks instead of where the mouse turned the camera (`CalcView`).
    look: Option<(f32, f32)>,
    /// Models the client asked pictures of, not made yet: the image each is for.
    icons: std::collections::VecDeque<(u32, String)>,
    gpu: Option<Gpu>,
    error: Option<String>,
    keys: HashSet<KeyCode>,
    /// The left and right mouse buttons held, while the mouse is captured.
    mouse: [bool; 2],
    /// The mouse wheel and the mouse's movement since the last command.
    wheel: i32,
    mouse_moved: [f64; 2],
    /// The physics gun holds something: the wheel and, with E, the mouse are its.
    holding: bool,
    captured: bool,
    /// The window has the keyboard.
    focused: bool,
    /// The game's menus show the cursor (Q, C); and whether the mouse was captured before.
    ui_cursor: bool,
    captured_before_ui: bool,
    /// Esc went down into the chat box: its going up goes there too.
    chat_esc: bool,
    last_frame: Instant,
    frames: u32,
    counting_since: Instant,
    /// F12 was pressed: save the next frame.
    screenshot: bool,
    /// How games start, and the main menu that starts them; it shows while there is no map,
    /// and over the game when `menu_open`.
    setup: Setup,
    menu: Option<menu::Menu>,
    menu_open: bool,
    /// The player's settings and key bindings.
    config: settings::Config,
    /// The console window opened the main menu over the game: it closes it again.
    console_menu: bool,
    /// How much of what the console said the console window has.
    console_seen: usize,
    /// Chromium, for the menu and the game's HTML panels.
    web: Option<Rc<gmod_web::Web>>,
    /// A map the menu asked for, loaded on the next frame (after one that says so).
    pending_map: Option<String>,
    /// A server to join, likewise.
    pending_connect: Option<String>,
    /// A map or a server's game being made ready in the background.
    loading: Option<Loading>,
    /// The menu said to quit.
    quit: bool,
    /// When the menu started, whether Chromium drew it, and whether it was said that it did not.
    menu_started: Instant,
    menu_drawn: bool,
    menu_late: bool,
    /// Frames drawn since the start.
    frames_drawn: u64,
    /// Steam, signed in as Garry's Mod.
    steam: Rc<gmod_steam::Steam>,
}

impl App {
    fn capture(&mut self, on: bool) {
        let Some(gpu) = &self.gpu else { return };
        if on {
            // locked where it can be, else kept inside the window
            let grabbed =
                gpu.window.set_cursor_grab(CursorGrabMode::Locked).or_else(|_| gpu.window.set_cursor_grab(CursorGrabMode::Confined));
            if grabbed.is_err() {
                return;
            }
        } else {
            let _ = gpu.window.set_cursor_grab(CursorGrabMode::None);
        }
        gpu.window.set_cursor_visible(!on);
        self.captured = on;
        if !on {
            self.mouse = [false; 2];
        }
    }

    /// A walking player with their feet at `feet`, if there is room for them there.
    fn stand(&self, feet: [f32; 3]) -> Option<Walker> {
        let collision = self.collision.as_ref().filter(|c| !c.is_empty())?;
        Player::spawn(&**collision, &self.settings, feet).map(Walker::new)
    }

    /// Back to where players start, walking if there is room; flying where it is when the map
    /// has no player start.
    fn respawn(&mut self) {
        let Some((origin, [pitch, yaw, _])) = self.spawn else {
            self.walker = None;
            return;
        };
        self.walker = self.stand(origin);
        self.camera.eye = self.walker.as_ref().map_or([origin[0], origin[1], origin[2] + self.settings.view_height], |w| w.player.eye());
        (self.camera.pitch, self.camera.yaw) = (pitch, yaw);
    }

    /// V: flying through walls, and walking again from where the eyes are, if there is room.
    fn toggle_noclip(&mut self) {
        if self.walker.take().is_some() {
            return;
        }
        let [x, y, z] = self.camera.eye;
        self.walker = self.stand([x, y, z - self.settings.view_height]);
        if self.walker.is_none() {
            say!("No room to stand here");
        }
    }

    /// The keys that move a walking player.
    fn movement_keys(&self) -> Keys {
        Keys {
            forward: self.bound("+forward"),
            back: self.bound("+back"),
            left: self.bound("+moveleft"),
            right: self.bound("+moveright"),
            jump: self.bound("+jump"),
            duck: self.bound("+duck"),
            run: self.bound("+speed"),
        }
    }

    /// The buttons held besides the walking keys.
    fn buttons(&self) -> Buttons {
        Buttons {
            attack: self.bound("+attack"),
            attack2: self.bound("+attack2"),
            reload: self.bound("+reload"),
            using: self.bound("+use"),
            walk: self.bound("+walk"),
            ..Buttons::default()
        }
    }

    /// Whether a key or mouse button held does `command` (`+forward`), as the player bound them.
    fn bound(&self, command: &str) -> bool {
        let does = |name: &str| self.config.command(name).is_some_and(|c| c.eq_ignore_ascii_case(command));
        self.keys.iter().filter_map(|&key| key_name(key)).any(does)
            || (self.mouse[0] && does("mouse1"))
            || (self.mouse[1] && does("mouse2"))
    }

    /// Whether a key does `command`, as the player bound it.
    fn key_does(&self, key: KeyCode, command: &str) -> bool {
        key_name(key).and_then(|name| self.config.command(name)).is_some_and(|c| c.eq_ignore_ascii_case(command))
    }

    /// A setting changed, or a game came: what it is for follows it.
    fn apply_setting(&mut self, name: &str) {
        let value = self.config.value(name).to_string();
        match name {
            "volume" => {
                if let Some(audio) = &mut self.audio {
                    audio.set_master(value.parse().unwrap_or(1.0));
                }
            }
            "mat_vsync" => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.set_vsync(value != "0");
                }
            }
            "opengmod_fullscreen" => {
                if let Some(gpu) = &self.gpu {
                    gpu.window.set_fullscreen((value != "0").then_some(Fullscreen::Borderless(None)));
                }
            }
            // the window's size, the video options' resolution
            "opengmod_width" | "opengmod_height" => {
                if let Some(gpu) = &self.gpu
                    && gpu.window.fullscreen().is_none()
                {
                    let (w, h) = (self.config.number("opengmod_width", 1280.0) as u32, self.config.number("opengmod_height", 720.0) as u32);
                    let _ = gpu.window.request_inner_size(winit::dpi::PhysicalSize::new(w.max(320), h.max(240)));
                }
            }
            // the menu speaks it too
            "gmod_language" => {
                if let Some(menu) = &self.menu {
                    menu.runtime().set_convar(name, &value);
                }
            }
            _ => {}
        }
        if settings::CLIENT_CONVARS.contains(&name)
            && let Some(game) = &self.game
        {
            game.client().set_convar(name, &value);
        }
    }

    /// A game came: the player's settings and key bindings are its client's.
    fn apply_to_game(&mut self) {
        for name in settings::CLIENT_CONVARS {
            self.apply_setting(name);
        }
        if let Some(game) = &self.game {
            game.client().set_binds(self.config.binds().to_vec());
        }
    }

    /// What the options window changed: kept, and followed at once.
    fn apply_setting_changes(&mut self, changes: Vec<gmod_lua::SettingChange>) {
        if changes.is_empty() {
            return;
        }
        let mut rebound = false;
        for change in changes {
            match change {
                gmod_lua::SettingChange::Value(name, value) => {
                    self.config.set(&name, &value);
                    self.apply_setting(&name);
                }
                gmod_lua::SettingChange::Bind(key, command) => {
                    self.config.bind(&key, command.as_deref());
                    rebound = true;
                }
            }
        }
        if rebound && let Some(game) = &self.game {
            game.client().set_binds(self.config.binds().to_vec());
        }
        self.config.save();
    }

    /// Moves the camera by the keys held, for the time since the last frame: the game runs
    /// with the player in it, or the player walks, or the camera flies.
    fn advance(&mut self, seconds: f32) {
        if self.game.is_some() {
            self.play(seconds);
            return;
        }
        let Some(collision) = self.collision.clone() else { return };
        let Some(mut walker) = self.walker.take() else {
            self.fly(seconds.min(0.1));
            return;
        };
        let command = self.movement_keys().command(self.camera.pitch, self.camera.yaw);
        self.camera.eye = walker.advance(&collision, &self.settings, &command, seconds);
        self.walker = Some(walker);
        // fell out of the map
        if self.camera.eye[2] < collision.mins[2] - 1024.0 {
            self.respawn();
        }
    }

    /// Whether the main menu shows: while there is no map, and over the game when opened.
    fn menu_shown(&self) -> bool {
        self.menu.is_some() && (self.menu_open || self.collision.is_none())
    }

    /// The pictures of models the client asked for (spawn icons the game has none of): those
    /// kept from before, else drawn now, a few milliseconds' worth a frame, and kept.
    fn make_icons(&mut self) {
        let Some(game) = &self.game else { return };
        self.icons.extend(game.client().take_model_icons());
        let (Some(models), Some(gpu)) = (&mut self.models, &mut self.gpu) else { return };
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(8) {
            let Some((id, model)) = self.icons.pop_front() else { break };
            let picture = icons::read(&model).or_else(|| {
                let renderer = gpu.renderer.as_mut()?;
                let (made, mins, maxs) = models.icon_model(&model, |world| renderer.add_model(&gpu.device, &gpu.queue, world))?;
                let picture = gpu.picture_model(made, &icons::camera(mins, maxs), icons::SIZE)?;
                icons::keep(&model, &picture);
                Some(picture)
            });
            if let Some(picture) = picture {
                game.client().give_picture(id, picture);
            }
        }
    }

    /// The camera the picture is drawn with: from the eyes, looking where the game's view says
    /// (`CalcView`), else where the mouse turned it.
    fn view_camera(&self) -> Camera {
        let mut camera = self.camera;
        if let Some((pitch, yaw)) = self.look {
            (camera.pitch, camera.yaw) = (pitch, yaw);
        }
        camera
    }

    /// Shows the menu over the game (Esc), or goes back to the game.
    /// Opens the console window, over the main menu, or closes it - and the menu, when it
    /// opened it over the game.
    fn toggle_console(&mut self) {
        let Some(menu) = &self.menu else { return };
        let shown = menu.runtime().toggle_console();
        if shown && !self.menu_shown() {
            self.toggle_menu();
            self.console_menu = self.menu_open;
        } else if !shown && self.console_menu {
            if self.menu_open {
                self.toggle_menu();
            }
            self.console_menu = false;
        }
    }

    fn toggle_menu(&mut self) {
        if self.menu.is_none() || self.collision.is_none() {
            return;
        }
        self.menu_open = !self.menu_open;
        if self.menu_open {
            self.capture(false);
            self.keys.clear();
        } else if self.focused && !self.ui_cursor {
            self.capture(true);
        }
    }

    /// Starts a game on a map, leaving the one there was, loading it here and now: the command
    /// line asked, before there is a window to show the loading in.
    fn enter_map(&mut self, map: &str) {
        self.leave_map();
        say!("Loading {map}");
        match load(&self.setup, map) {
            Ok(loaded) => self.start_game(loaded),
            Err(e) => say_error!("{e}"),
        }
    }

    /// The game starts on a map loaded: the player's own, with others joining over the network
    /// when it takes more than one.
    fn start_game(&mut self, loaded: Loaded) {
        let pages = self.web.clone().map(menu::web_pages);
        let setup = &self.setup;
        // more than one player: the others join over the network
        let listen = (setup.max_players > 1).then(|| SocketAddr::from(([0, 0, 0, 0], setup.port)));
        let game = loaded.lua.as_ref().and_then(|(vfs, map)| {
            let started = Instant::now();
            let player = (setup.name.as_str(), setup.identity.public());
            match Game::start(vfs.clone(), map, &setup.gamemode, pages, setup.max_players, listen, player) {
                Ok(game) => {
                    say!("The game started in {:.1} s", started.elapsed().as_secs_f64());
                    Some(game)
                }
                Err(e) => {
                    say!("{e}\nThe map is walked without the game's Lua.");
                    None
                }
            }
        });
        self.show_map(loaded, game);
    }

    /// Joins a server over the network, here and now (the command line asked): what it says
    /// to have is fetched, its map loaded, and the player plays there.
    fn connect(&mut self, address: &str) {
        self.leave_map();
        let joined = prepare_join(&self.setup, address, &|status| say!("{status}")).and_then(|j| self.join_game(j));
        if let Err(e) = joined {
            say_error!("Could not join {address}: {e}");
        }
    }

    /// The player joins the server a [`Joining`] got ready for.
    fn join_game(&mut self, joining: Joining) -> Result<(), String> {
        let Joining { loaded, connection, addr } = joining;
        let pages = self.web.clone().map(menu::web_pages);
        let (vfs, map) = loaded.lua.as_ref().ok_or("no Lua")?;
        let game = Game::join(vfs.clone(), map, connection, pages)?;
        self.show_map(loaded, Some(game));
        if let Some(menu) = &self.menu {
            menu.on_server(Some(&addr.to_string()));
        }
        Ok(())
    }

    /// Starts loading a map (`Some`) or joining a server in the background: the window goes on,
    /// the menu showing its loading screen, until it is ready ([`Self::poll_loading`]).
    fn begin_loading(&mut self, what: Loadable) {
        self.leave_map();
        let (to_main, done) = std::sync::mpsc::channel();
        let status = Arc::new(std::sync::Mutex::new(String::new()));
        let details = Arc::new(std::sync::Mutex::new(None));
        let setup = self.setup.clone();
        let (said, told) = (status.clone(), details.clone());
        let hosting = matches!(what, Loadable::Map(_));
        let name = match &what {
            Loadable::Map(map) => map.clone(),
            Loadable::Server(address) => address.clone(),
        };
        std::thread::spawn(move || {
            let say = |text: &str| {
                say!("{text}");
                if let Ok(mut status) = said.lock() {
                    *status = text.to_string();
                }
            };
            let prepared = match what {
                Loadable::Map(map) => {
                    say(&format!("Loading {map}"));
                    load(&setup, &map).map(|loaded| Prepared::Map(Box::new(loaded)))
                }
                Loadable::Server(address) => prepare_join_telling(&setup, &address, &say, &told).map(|j| Prepared::Server(Box::new(j))),
            };
            let _ = to_main.send(prepared);
        });
        if let Some(menu) = &self.menu {
            // the loading screen of one's own game names its map from `host_map`
            if hosting {
                menu.runtime().run_command(&format!("host_map \"{}.bsp\"", name.trim_end_matches(".bsp")));
                menu.runtime().run_command(&format!("gamemode \"{}\"", self.setup.gamemode));
            }
            menu.runtime().begin_loading(hosting);
        }
        self.loading = Some(Loading { done, status, details, name });
    }

    /// The background loading: the loading screen told what it does, and, once it is ready,
    /// the game goes on with it - or the menu comes back, saying why not.
    fn poll_loading(&mut self) {
        let Some(loading) = &self.loading else { return };
        if let Some(menu) = &self.menu {
            if let Ok(status) = loading.status.lock() {
                menu.runtime().set_load_status(&status);
            }
            if let Some(details) = loading.details.lock().ok().and_then(|mut d| d.take()) {
                menu.runtime().game_details(&details.server, &details.loading_url, &details.map, details.max_players, &details.gamemode);
                menu.runtime().set_downloadables(details.downloads);
            }
        }
        let result = match loading.done.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the loading stopped".to_string()),
        };
        let name = loading.name.clone();
        self.loading = None;
        let started = match result {
            Ok(Prepared::Map(loaded)) => {
                self.start_game(*loaded);
                Ok(())
            }
            Ok(Prepared::Server(joining)) => self.join_game(*joining),
            Err(e) => Err(e),
        };
        if let Some(menu) = &self.menu {
            menu.runtime().end_loading();
        }
        if let Err(e) = started {
            say_error!("{name}: {e}");
        }
    }

    /// A map loaded comes on the screen, with its game if there is one.
    fn show_map(&mut self, loaded: Loaded, game: Option<Game>) {
        self.game = game;
        self.models = self.game.as_ref().and(loaded.lua.as_ref()).map(|(vfs, _)| Models::new(vfs.clone()).with_brushes(loaded.brushes));
        self.audio = self.game.as_ref().and(loaded.lua.as_ref()).map(|(vfs, _)| audio::Audio::new(vfs.clone()));
        self.apply_to_game();
        self.collision = Some(loaded.collision);
        self.light = Some(loaded.light);
        self.lit.clear();
        self.spawn = loaded.spawn;
        self.walker = None;
        self.name = loaded.name;
        self.camera = loaded.camera;
        self.status.clear();
        match &mut self.gpu {
            Some(gpu) => gpu.set_world(Some(&loaded.world)),
            None => self.world = Some(loaded.world),
        }
        self.menu_open = false;
        if let Some(menu) = &self.menu {
            menu.runtime().set_in_game(true);
        }
        if self.game.is_some() {
            // the eyes and angles of the player where the game spawned them
            self.play(0.0);
        } else {
            // walking from the player start, as the game begins
            self.respawn();
            if self.walker.is_none() {
                say!("No player start with room to stand: flying (V walks from where you are)");
            }
        }
        if self.focused && self.gpu.is_some() {
            self.capture(true);
        }
        self.last_frame = Instant::now();
    }

    /// Leaves the game: back to the menu.
    fn leave_map(&mut self) {
        if self.collision.is_none() {
            return;
        }
        if let Some(game) = &mut self.game {
            game.leave();
        }
        self.game = None;
        self.models = None;
        self.audio = None;
        self.collision = None;
        self.light = None;
        self.lit.clear();
        self.walker = None;
        self.look = None;
        self.icons.clear();
        self.ui_cursor = false;
        self.name = "main menu".to_string();
        if let Some(gpu) = &mut self.gpu {
            gpu.set_world(None);
        }
        self.capture(false);
        if let Some(menu) = &self.menu {
            menu.runtime().set_in_game(false);
            menu.on_server(None);
        }
    }

    /// Says once how long Chromium took to draw the menu, or that it has not after a while:
    /// what to look at when the menu does not show.
    fn report_menu(&mut self) {
        let Some(menu) = &self.menu else { return };
        if self.menu_drawn {
            return;
        }
        let (pages, drawn) = menu.runtime().page_counts();
        let seconds = self.menu_started.elapsed().as_secs_f64();
        if drawn > 0 {
            self.menu_drawn = true;
            say!("Chromium drew the main menu in {seconds:.1} s");
        } else if seconds > 10.0 && !self.menu_late {
            self.menu_late = true;
            say!(
                "Chromium has not drawn the main menu in 10 s ({pages} pages, {} frames drawn); \
                 its log is in ~/.opengmod/logs/chromium.log. opengmod --no-html goes without it.",
                self.frames_drawn
            );
        }
    }

    /// What Steam said this frame, to the menu; subscribing to an item or leaving one changes
    /// the addons it lists (and mounts from the next game on).
    fn steam_frame(&mut self) {
        use gmod_steam::Event;
        let events = self.steam.run_frame();
        if let Some(menu) = &self.menu {
            menu.runtime().steam_events(&events);
        }
        let changed = events.iter().any(|event| {
            matches!(event, Event::Subscribed { result: 1, .. } | Event::Unsubscribed { result: 1, .. } | Event::Installed { .. })
        });
        if !changed {
            return;
        }
        self.setup.subscribed = Some(self.steam.subscribed(true).into_iter().collect());
        if let Some(menu) = &self.menu
            && let Ok(install) = Install::locate(self.setup.gmod.as_deref())
        {
            menu.runtime().addons_changed(menu::addons(&install, &self.setup.disabled, self.setup.subscribed.as_ref()));
        }
        self.sync_addons();
    }

    /// The addons switched on, off, subscribed to or downloaded are mounted or taken out at
    /// once, in the menu and in the game running, as GMod does: their content is there from
    /// now on (their Lua runs from the next map on).
    fn sync_addons(&mut self) {
        let Ok(install) = Install::locate(self.setup.gmod.as_deref()) else { return };
        let options = self.setup.mount_options();
        let mut systems: Vec<Arc<Vfs>> = Vec::new();
        for vfs in self.menu.as_ref().map(menu::Menu::vfs).into_iter().chain(self.models.as_ref().map(Models::vfs)) {
            if !systems.iter().any(|known| Arc::ptr_eq(known, &vfs)) {
                systems.push(vfs);
            }
        }
        let mut changed = false;
        for vfs in &systems {
            let changes = gmod_fs::mount::sync_addons(vfs, &install, &options);
            changed |= !changes.mounted.is_empty() || changes.unmounted > 0;
            for title in &changes.mounted {
                say!("Mounted {title}");
            }
            for warning in &changes.warnings {
                say_error!("{warning}");
            }
        }
        if !changed {
            return;
        }
        if let Some(menu) = &self.menu {
            menu.runtime().content_changed();
        }
        if let Some(models) = &mut self.models {
            models.forget_missing();
        }
        if let Some(audio) = &mut self.audio {
            audio.forget_missing();
        }
        if let Some(game) = &self.game {
            game.content_changed();
        }
    }

    /// A frame of the main menu: Chromium's work, the menu's Lua when it shows, and what it
    /// asked of the engine.
    fn menu_frame(&mut self, seconds: f32) {
        self.report_menu();
        let Some(menu) = &self.menu else { return };
        // the console window: what was said since, and what was typed into it
        for (text, color) in console::since(&mut self.console_seen) {
            menu.runtime().console_print(&text, color);
        }
        for line in menu.runtime().take_console_commands() {
            console::type_line(line);
        }
        if let Some(line) = menu.runtime().take_console_complete() {
            let mut list = self.game.as_ref().map(|game| game.completions(&line)).unwrap_or_default();
            list.extend(menu.runtime().completions(&line));
            menu.runtime().console_completions(&line, console::completions(&line, list));
        }
        if self.menu_shown()
            && let Some(gpu) = &self.gpu
        {
            menu.frame(gpu.config.width, gpu.config.height, f64::from(seconds));
        }
        menu.browse();
        if self.game.is_none() {
            for line in game::typed_lines() {
                menu.console(&line);
            }
        }
        let addons_changed = menu.runtime().take_addon_changes();
        let setting_changes = menu.runtime().take_setting_changes();
        if let Some(addons) = &addons_changed {
            self.setup.disabled = addons.iter().filter(|a| !a.mounted).map(menu::addon_key).collect();
            let on = addons.iter().filter(|a| a.mounted).count();
            say!("Addons: {on} on, {} off", addons.len() - on);
            if let Some(file) = menu::disabled_file()
                && let Err(e) = gmod_fs::workshop::write_disabled(&file, &self.setup.disabled)
            {
                say_error!("{}: {e}", file.display());
            }
        }
        for line in menu.take_commands() {
            let words: Vec<&str> = line.split_whitespace().map(|w| w.trim_matches('"')).collect();
            match words.as_slice() {
                ["map", map, ..] => self.pending_map = Some(map.to_string()),
                ["connect", address, ..] => self.pending_connect = Some(address.to_string()),
                ["maxplayers", n, ..] => {
                    if let Some(n) = n.parse::<u32>().ok().filter(|n| (1..=gmod_lua::player::MAX_PLAYERS).contains(n)) {
                        self.setup.max_players = n;
                    }
                }
                ["gamemode", name, ..] => {
                    self.setup.gamemode = name.to_string();
                    if let Some(menu) = &self.menu {
                        menu.runtime().set_gamemode(name);
                    }
                }
                ["disconnect", ..] => self.leave_map(),
                ["quit" | "exit", ..] => self.quit = true,
                ["resume", ..] => {
                    if self.menu_open {
                        self.toggle_menu();
                    }
                }
                // what a new game is set up with that openGMOD has no use for yet
                ["progress_enable" | "sv_cheats" | "hostname" | "p2p_enabled" | "p2p_friendsonly" | "sv_lan" | "commentary", ..] => {}
                _ => match &self.game {
                    Some(game) => game.command(&line),
                    None => say!("Unknown command \"{line}\""),
                },
            }
            if self.menu.is_none() {
                break;
            }
        }
        if addons_changed.is_some() {
            self.sync_addons();
        }
        self.apply_setting_changes(setting_changes);
    }

    /// The game's menus show the cursor or hide it again: the mouse is let go and taken back.
    fn follow_ui_cursor(&mut self) {
        let visible = self.game.as_ref().is_some_and(Game::cursor_visible);
        if visible == self.ui_cursor {
            return;
        }
        self.ui_cursor = visible;
        if visible {
            self.captured_before_ui = self.captured;
            self.capture(false);
            // the cursor starts in the middle, as the game puts it
            if let (Some(gpu), Some(game)) = (&self.gpu, &self.game) {
                let (x, y) = (gpu.config.width / 2, gpu.config.height / 2);
                let _ = gpu.window.set_cursor_position(winit::dpi::PhysicalPosition::new(x, y));
                game.cursor_moved(x as i32, y as i32);
            }
        } else if self.captured_before_ui && self.focused {
            self.capture(true);
        }
    }

    /// What the program does itself of a line typed - into the terminal, the console window;
    /// the rest is for the player's console, given back.
    fn typed_line(&mut self, line: String) -> Option<String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["connect", address] => self.pending_connect = Some(address.to_string()),
            ["disconnect"] => self.leave_map(),
            ["quit" | "exit"] => self.quit = true,
            ["screenshot" | "jpeg"] => self.screenshot = true,
            ["toggleconsole"] => self.toggle_console(),
            // the menu over the game, as Escape shows and hides it
            ["gameui_activate"] => {
                if !self.menu_open {
                    self.toggle_menu();
                }
            }
            ["gameui_hide"] => {
                if self.menu_open {
                    self.toggle_menu();
                }
            }
            ["showconsole"] => {
                if !self.menu.as_ref().is_some_and(|menu| menu.runtime().console_shown()) {
                    self.toggle_console();
                }
            }
            ["lua_run_menu", ..] => {
                if let Some(menu) = &self.menu {
                    menu.console(&line);
                }
            }
            // Lua for the player's own client, typed here only - where the server allows it
            ["lua_run_cl", ..] => {
                if let (Some(game), Some(code)) = (&self.game, line.trim_start().get("lua_run_cl ".len()..)) {
                    if game.own_lua_allowed() {
                        let _ = game.client().run_string(code, "lua_run_cl");
                    } else {
                        say!("lua_run_cl: the server does not let players run Lua of their own (sv_allowcslua 0)");
                    }
                }
            }
            _ => return Some(line),
        }
        None
    }

    /// The lines typed while the menu shows over a game that waits: they run at once, as the
    /// engine's console runs them over a paused game.
    fn typed_while_paused(&mut self) {
        for line in game::typed_lines() {
            if let Some(line) = self.typed_line(line)
                && let Some(game) = &self.game
            {
                game.command(&line);
            }
        }
    }

    /// A frame of the game: the player's command, the ticks, and the eyes to see from.
    fn play(&mut self, seconds: f32) {
        // under the menu nothing is pressed
        let (keys, mut buttons) =
            if self.menu_shown() { (Keys::default(), Buttons::default()) } else { (self.movement_keys(), self.buttons()) };
        buttons.wheel = std::mem::take(&mut self.wheel);
        buttons.mouse = self.mouse_moved.map(|counts| counts.trunc() as i32);
        self.mouse_moved = self.mouse_moved.map(f64::fract);
        // what the program does itself of the lines typed; the rest is the player's console
        let mut lines = Vec::new();
        for line in game::typed_lines() {
            if let Some(line) = self.typed_line(line) {
                lines.push(line);
            }
            if self.game.is_none() {
                return;
            }
        }
        let Some(game) = &mut self.game else { return };
        game.type_lines(lines);
        let shown = game.frame(&keys, &buttons, self.camera.pitch, self.camera.yaw, f64::from(seconds));
        if let Some(reason) = game.closed() {
            say!("Disconnected: {reason}");
            self.leave_map();
            return;
        }
        let Some(shown) = shown else { return };
        self.holding = shown.view.holding;
        self.camera.eye = shown.eye;
        self.look = shown.look;
        self.camera.fov = shown.fov;
        if let Some((pitch, yaw)) = shown.turned {
            (self.camera.pitch, self.camera.yaw) = (pitch, yaw);
        }
        self.status = game::status(&shown.view);
        // fell out of the map: as if into a pit
        if let Some(collision) = self.collision.as_ref().filter(|c| !c.is_empty())
            && shown.view.alive
            && shown.eye[2] < collision.mins[2] - 1024.0
        {
            game.command("kill");
        }
        // the lights of the moment: the client's, and the flashlight from the eyes
        if let Some(renderer) = self.gpu.as_mut().and_then(|gpu| gpu.renderer.as_mut()) {
            let points = game
                .dynamic_lights()
                .iter()
                .map(|l| gmod_render::PointLight {
                    position: [l.position.x, l.position.y, l.position.z],
                    color: l.color,
                    radius: l.radius,
                    world: l.world,
                    models: l.models,
                })
                .collect();
            let spot = shown.view.flashlight.then(|| flashlight(&self.camera));
            renderer.set_lights(points, spot);
        }
        // what the game played this frame, heard from the eyes
        if let Some(audio) = &mut self.audio {
            audio.handle(game.take_sounds(), |entity| game.entity_position(entity));
            audio.listen(self.camera.eye, self.camera.yaw, |entity| game.entity_position(entity));
        }
    }

    /// A key pressed while the game runs: what the game binds to it. Whether it was one.
    fn game_key(&mut self, key: KeyCode) -> bool {
        let Some(command) = key_name(key).and_then(|name| self.config.command(name)).map(str::to_lowercase) else { return false };
        let Some(game) = &mut self.game else { return false };
        let words: Vec<&str> = command.split_whitespace().collect();
        match words.as_slice() {
            // slot1 the first, slot0 the tenth
            [slot] if slot.starts_with("slot") && slot[4..].parse::<i64>().is_ok() => {
                let n: i64 = slot[4..].parse().unwrap_or(1);
                game.choose_weapon(Choose::Slot(if n == 0 { 9 } else { n - 1 }));
            }
            ["invnext"] => game.choose_weapon(Choose::Next),
            ["invprev"] => game.choose_weapon(Choose::Previous),
            ["impulse", "100"] => game.flashlight(),
            ["noclip" | "gmod_undo" | "lastinv" | "kill" | "undo"] => game.command(&command),
            _ => return false,
        }
        true
    }

    /// Flies the camera by the keys held, through walls.
    fn fly(&mut self, seconds: f32) {
        let keys = self.movement_keys();
        let forward = self.camera.forward();
        let right = self.camera.right();
        let mut step = [0.0f32; 3];
        let mut push = |direction: [f32; 3], amount: f32| {
            for (s, d) in step.iter_mut().zip(direction) {
                *s += d * amount;
            }
        };
        push(forward, f32::from(keys.forward) - f32::from(keys.back));
        push(right, f32::from(keys.right) - f32::from(keys.left));
        push([0.0, 0.0, 1.0], f32::from(keys.jump) - f32::from(keys.duck));
        let fast = if keys.run { 4.0 } else { 1.0 };
        for (eye, s) in self.camera.eye.iter_mut().zip(step) {
            *eye += s * SPEED * fast * seconds;
        }
    }

    /// Gives the renderer the models of the game's entities and where they are now.
    fn show_entities(&mut self) {
        let camera = self.view_camera();
        let (Some(game), Some(models), Some(gpu)) = (&self.game, &mut self.models, &mut self.gpu) else { return };
        let Some(renderer) = &mut gpu.renderer else { return };
        let mut placed = models.place(&game.drawn(), |model| renderer.add_model(&gpu.device, &gpu.queue, model));
        // the light where each stands, worked out once for every 8 units
        if self.lit.len() > 8192 {
            self.lit.clear();
        }
        // what the player holds, lit as the eyes are
        let (held, fov) = game.view_models(camera.eye, camera.pitch, camera.yaw);
        let mut view_models = models.place(&held, |model| renderer.add_model(&gpu.device, &gpu.queue, model));
        // what the game drew in the world by itself this frame: beams, sprites, quads
        let primitives = game.client().take_primitives();
        // the pictures drawn in the world, given the renderer the first time
        for name in primitives.iter().map(|p| &p.material.name) {
            let Some(id) = name.strip_prefix(gmod_lua::render3d::IMAGES).and_then(|id| id.parse().ok()) else { continue };
            if renderer.has_texture(name) {
                continue;
            }
            let limit = gpu.device.limits().max_texture_dimension_2d;
            // one that could not be read is plain
            let (width, height, pixels) = match game.client().image(id).and_then(|(_, pixels)| pixels) {
                Some(p) if p.width > 0 && p.height > 0 && p.width <= limit && p.height <= limit => (p.width, p.height, p.pixels),
                _ => (1, 1, vec![255; 4]),
            };
            renderer.set_effect_pixels(&gpu.device, &gpu.queue, name, width, height, pixels);
        }
        // the pages of glyphs and the pictures the renderer has, which text and pictures in
        // the world are cut from
        let pages: HashSet<String> = primitives
            .iter()
            .map(|p| &p.material.name)
            .filter(|name| effects::given(name) && renderer.has_texture(name))
            .cloned()
            .collect();
        let batches = effects::batches(
            &primitives,
            &camera,
            &mut models.materials,
            |texture| renderer.add_effect_texture(&gpu.device, &gpu.queue, texture),
            |name| pages.contains(name),
        );
        renderer.set_effects(batches);
        let (Some(light), Some(collision)) = (&self.light, &self.collision) else { return };
        for p in placed.iter_mut().chain(&mut view_models) {
            let key = p.origin.map(|v| (v / 8.0).round() as i32);
            p.light = self.lit.entry(key).or_insert_with(|| light.at(collision, p.origin)).clone();
        }
        renderer.set_entities(placed);
        renderer.set_view_models(view_models, fov);
    }

    fn count_frame(&mut self) {
        self.frames += 1;
        let elapsed = self.counting_since.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            if let Some(gpu) = &self.gpu {
                let [x, y, z] = self.camera.eye.map(|v| v.round() as i32);
                let fps = self.frames as f32 / elapsed;
                let title = if self.menu_shown() {
                    format!("openGMOD - main menu - {fps:.0} fps")
                } else if self.game.is_some() {
                    format!("openGMOD - {} - {fps:.0} fps - {}", self.name, self.status)
                } else {
                    let mode = if self.walker.is_some() { "" } else { " - noclip" };
                    format!("openGMOD - {} - {fps:.0} fps - {x} {y} {z}{mode}", self.name)
                };
                gpu.window.set_title(&title);
            }
            self.frames = 0;
            self.counting_since = Instant::now();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let (vsync, fullscreen) = (self.config.number("mat_vsync", 1.0) != 0.0, self.config.number("opengmod_fullscreen", 0.0) != 0.0);
        let size = (self.config.number("opengmod_width", 1280.0) as u32, self.config.number("opengmod_height", 720.0) as u32);
        let renderer = self.config.value("opengmod_renderer").to_string();
        match Gpu::new(event_loop, &format!("openGMOD - {}", self.name), vsync, fullscreen, size, &renderer) {
            Ok(mut gpu) => {
                // what draws, for the advanced video options
                if let Some(menu) = &self.menu {
                    let available = gpu.backends.iter().map(|&b| renderer_name(b).to_string()).collect();
                    menu.runtime().set_renderer(renderer_name(gpu.backend), &gpu.adapter_name, available);
                }
                // the screen's sizes, for the video options
                if let (Some(menu), Some(monitor)) = (&self.menu, gpu.window.current_monitor()) {
                    let mut modes: Vec<(u32, u32)> = monitor
                        .video_modes()
                        .map(|mode| (mode.size().width, mode.size().height))
                        .filter(|&(w, h)| w >= 640 && h >= 480)
                        .collect();
                    modes.sort_unstable();
                    modes.dedup();
                    menu.runtime().set_video_modes(modes, (monitor.size().width, monitor.size().height));
                }
                if let Some(world) = self.world.take() {
                    gpu.set_world(Some(&world));
                }
                gpu.window.request_redraw();
                self.gpu = Some(gpu);
                self.last_frame = Instant::now();
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.keys.clear();
                    self.capture(false);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(menu) = self.menu.as_ref().filter(|_| self.menu_shown()) {
                    menu.runtime().set_cursor(position.x as i32, position.y as i32);
                } else if let Some(game) = &self.game {
                    game.cursor_moved(position.x as i32, position.y as i32);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if let Some(menu) = self.menu.as_ref().filter(|_| self.menu_shown()) {
                    if let Some(code) = mouse_code(button) {
                        menu.runtime().button(code, pressed);
                    }
                    return;
                }
                // the game's menus have the mouse while they show the cursor
                if let Some(game) = &self.game
                    && self.ui_cursor
                {
                    if let Some(code) = mouse_code(button) {
                        game.button(code, pressed);
                    }
                    return;
                }
                let index = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    _ => return,
                };
                if pressed && !self.captured {
                    // the click that takes the mouse does not fire
                    if index == 0 {
                        self.capture(true);
                    }
                } else if let Some(game) = self.game.as_ref().filter(|g| pressed && g.client().choosing_weapon()) {
                    // while the row of weapon slots shows, a click chooses or puts it away
                    if index == 0 {
                        game.take_chosen_weapon();
                    } else {
                        game.client().cancel_weapon_choice();
                    }
                } else if self.captured || !pressed {
                    self.mouse[index] = pressed;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let up = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y > 0.0,
                    MouseScrollDelta::PixelDelta(p) => p.y > 0.0,
                };
                let code = if up { MOUSE_WHEEL_UP } else { MOUSE_WHEEL_UP + 1 };
                if let Some(menu) = self.menu.as_ref().filter(|_| self.menu_shown()) {
                    menu.runtime().button(code, true);
                    menu.runtime().button(code, false);
                } else if let Some(game) = &self.game
                    && self.ui_cursor
                {
                    game.button(code, true);
                    game.button(code, false);
                } else if let Some(game) = &self.game
                    && self.captured
                {
                    if self.holding {
                        self.wheel += if up { 1 } else { -1 };
                    } else {
                        game.choose_weapon(if up { Choose::Previous } else { Choose::Next });
                    }
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(key), state, repeat, text, .. }, .. } => {
                let pressed = state == ElementState::Pressed;
                // Esc closes the chat box first; its going up follows it there, though the box is
                // shut by then, so the game does not think it still held
                if key == KeyCode::Escape
                    && !self.menu_shown()
                    && let Some(game) = self.game.as_ref().filter(|game| game.chat_open() || (!pressed && self.chat_esc))
                {
                    self.chat_esc = pressed;
                    if let Some(code) = gmod_key(key) {
                        game.button(code, pressed);
                    }
                    return;
                }
                // `: the console window, over the main menu as GMod opens it, and away again
                if (key == KeyCode::Backquote || self.key_does(key, "toggleconsole"))
                    && self.menu.is_some()
                    && !self.game.as_ref().is_some_and(Game::chat_open)
                {
                    if pressed && !repeat {
                        self.toggle_console();
                    }
                    return;
                }
                // Esc: the main menu over the game, and back
                if key == KeyCode::Escape && self.menu.is_some() && self.collision.is_some() {
                    if pressed && !repeat {
                        self.toggle_menu();
                    }
                    return;
                }
                // the menu has the keys while it shows, but for full screen and screenshots
                if let Some(menu) = self.menu.as_ref().filter(|_| self.menu_shown())
                    && !matches!(key, KeyCode::F11 | KeyCode::F12)
                {
                    if let Some(code) = gmod_key(key) {
                        menu.runtime().button(code, pressed);
                    }
                    if pressed && let Some(text) = &text {
                        menu.runtime().type_text(text);
                    }
                    return;
                }
                // the game's client first: its menus, its text entries, its own commands (Q, C)
                if let Some(game) = &self.game
                    && key != KeyCode::Escape
                {
                    let mut taken = gmod_key(key).is_some_and(|code| game.button(code, pressed));
                    if pressed && let Some(text) = &text {
                        taken |= game.type_text(text);
                    }
                    if taken && pressed {
                        return;
                    }
                }
                if pressed {
                    match key {
                        KeyCode::Escape => self.capture(false),
                        _ if !repeat && self.game_key(key) => {}
                        _ if !repeat && self.key_does(key, "noclip") => self.toggle_noclip(),
                        _ if !repeat && (self.key_does(key, "jpeg") || self.key_does(key, "screenshot")) => self.screenshot = true,
                        KeyCode::F12 if !repeat => self.screenshot = true,
                        KeyCode::F11 if !repeat => {
                            if let Some(gpu) = &self.gpu {
                                let full = gpu.window.fullscreen().is_some();
                                gpu.window.set_fullscreen(if full { None } else { Some(Fullscreen::Borderless(None)) });
                                self.config.set("opengmod_fullscreen", if full { "0" } else { "1" });
                                self.config.save();
                                if let Some(menu) = &self.menu {
                                    menu.runtime().set_settings(self.config.values(), self.config.binds().to_vec());
                                }
                            }
                        }
                        _ => {}
                    }
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(map) = self.pending_map.take() {
                    self.begin_loading(Loadable::Map(map));
                }
                if let Some(address) = self.pending_connect.take() {
                    self.begin_loading(Loadable::Server(address));
                }
                self.poll_loading();
                // fps_max: no more frames a second than the player wants
                let fps_max = self.config.number("fps_max", 0.0);
                if fps_max >= 10.0 {
                    let frame = Duration::from_secs_f32(1.0 / fps_max);
                    let spent = self.last_frame.elapsed();
                    if spent < frame {
                        std::thread::sleep(frame - spent);
                    }
                }
                let now = Instant::now();
                let seconds = (now - self.last_frame).as_secs_f32().min(0.25);
                self.last_frame = now;
                // a single-player game waits while the menu shows over it; one with others in it
                // goes on
                if !self.menu_shown() || self.game.as_ref().is_some_and(Game::multiplayer) {
                    self.advance(seconds);
                } else if self.game.is_some() {
                    self.typed_while_paused();
                }
                self.show_entities();
                self.make_icons();
                if let (Some(game), Some(gpu)) = (&self.game, &self.gpu) {
                    game.paint(gpu.config.width, gpu.config.height, f64::from(seconds));
                }
                self.steam_frame();
                self.menu_frame(seconds);
                if !self.menu_shown() {
                    self.follow_ui_cursor();
                }
                let menu = self.menu.as_ref().filter(|_| self.menu_shown()).map(menu::Menu::runtime);
                let camera = self.view_camera();
                if let Some(gpu) = &mut self.gpu {
                    let client = self.game.as_ref().map(Game::client);
                    if gpu.draw(&camera, client, menu) {
                        self.frames_drawn += 1;
                    }
                    if std::mem::take(&mut self.screenshot) {
                        match gpu.screenshot(&camera, client, menu, &self.name) {
                            Ok(file) => say!("Screenshot: {}", file.display()),
                            Err(e) => say!("No screenshot: {e}"),
                        }
                    }
                    gpu.window.request_redraw();
                }
                self.count_frame();
                if self.quit {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _device: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
            && self.captured
        {
            self.mouse_moved = [self.mouse_moved[0] + dx, self.mouse_moved[1] + dy];
            // E with the physics gun holding something: the mouse turns that, not the view
            if !(self.holding && self.bound("+use")) {
                // degrees a count, as the engine has them: sensitivity by m_yaw and m_pitch
                let sensitivity = self.config.number("sensitivity", 3.0);
                let (yaw, pitch) = (sensitivity * self.config.number("m_yaw", 0.022), sensitivity * self.config.number("m_pitch", 0.022));
                self.camera.yaw -= dx as f32 * yaw;
                self.camera.pitch = (self.camera.pitch + dy as f32 * pitch).clamp(-89.0, 89.0);
            }
        }
    }
}

/// The flashlight, as the engine's: from just below and right of the eyes along the view, a
/// cone of 45 degrees (`r_flashlightfov`) reaching 750 units (`r_flashlightfar`).
fn flashlight(camera: &Camera) -> gmod_render::SpotLight {
    let forward = camera.forward();
    let right = camera.right();
    let position = std::array::from_fn(|i| camera.eye[i] + right[i] * 6.0 - if i == 2 { 4.0 } else { 0.0 });
    gmod_render::SpotLight {
        position,
        direction: forward,
        color: [2.5, 2.5, 2.3],
        far: 750.0,
        outer: 22.5f32.to_radians().cos(),
        inner: 15f32.to_radians().cos(),
    }
}

/// `MOUSE_LEFT` and `MOUSE_WHEEL_UP`; the other buttons follow them.
const MOUSE_LEFT: i32 = 107;
const MOUSE_WHEEL_UP: i32 = 112;

/// The game's number of a mouse button (`MOUSE_*`).
fn mouse_code(button: MouseButton) -> Option<i32> {
    Some(match button {
        MouseButton::Left => MOUSE_LEFT,
        MouseButton::Right => MOUSE_LEFT + 1,
        MouseButton::Middle => MOUSE_LEFT + 2,
        MouseButton::Back => MOUSE_LEFT + 3,
        MouseButton::Forward => MOUSE_LEFT + 4,
        MouseButton::Other(_) => return None,
    })
}

/// The engine's name of a key of the keyboard (`w`, `SPACE`), as binds name it.
fn key_name(key: KeyCode) -> Option<&'static str> {
    gmod_key(key).and_then(gmod_lua::client::key_name)
}

/// The game's number of a key (`KEY_*`), by where it is on the keyboard.
fn gmod_key(key: KeyCode) -> Option<i32> {
    use KeyCode::*;
    const DIGITS: [KeyCode; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    const LETTERS: [KeyCode; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV,
        KeyW, KeyX, KeyY, KeyZ,
    ];
    const PAD: [KeyCode; 10] = [Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9];
    const F: [KeyCode; 12] = [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12];
    let find = |list: &[KeyCode], first: i32| list.iter().position(|&k| k == key).map(|i| first + i as i32);
    if let Some(code) = find(&DIGITS, 1).or_else(|| find(&LETTERS, 11)).or_else(|| find(&PAD, 37)).or_else(|| find(&F, 92)) {
        return Some(code);
    }
    Some(match key {
        NumpadDivide => 47,
        NumpadMultiply => 48,
        NumpadSubtract => 49,
        NumpadAdd => 50,
        NumpadEnter => 51,
        NumpadDecimal => 52,
        BracketLeft => 53,
        BracketRight => 54,
        Semicolon => 55,
        Quote => 56,
        Backquote => 57,
        Comma => 58,
        Period => 59,
        Slash => 60,
        Backslash => 61,
        Minus => 62,
        Equal => 63,
        Enter => 64,
        Space => 65,
        Backspace => 66,
        Tab => 67,
        CapsLock => 68,
        NumLock => 69,
        Escape => 70,
        ScrollLock => 71,
        Insert => 72,
        Delete => 73,
        Home => 74,
        End => 75,
        PageUp => 76,
        PageDown => 77,
        Pause => 78,
        ShiftLeft => 79,
        ShiftRight => 80,
        AltLeft => 81,
        AltRight => 82,
        ControlLeft => 83,
        ControlRight => 84,
        SuperLeft => 85,
        SuperRight => 86,
        ContextMenu => 87,
        ArrowUp => 88,
        ArrowLeft => 89,
        ArrowDown => 90,
        ArrowRight => 91,
        _ => return None,
    })
}

/// Whether Windows Explorer started the program (no shell around it): then the window of a
/// failed start stays until Enter, or the message would be gone at once.
fn started_from_explorer() -> bool {
    let from_shell = ["PROMPT", "WT_SESSION", "TERM_PROGRAM", "TERM", "MSYSTEM"].iter().any(|v| std::env::var_os(v).is_some());
    cfg!(windows) && !from_shell
}

fn fail(message: &str) -> ExitCode {
    if !terminal::has_output() {
        terminal::message("openGMOD", message);
        return ExitCode::FAILURE;
    }
    say_error!("{message}");
    if started_from_explorer() {
        say_error!("\nPress Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    // Chromium starts the program again for its own processes: those run here and go
    if let Some(code) = gmod_web::subprocess() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            terminal::open();
            say!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            terminal::open();
            return fail(&format!("{e}\n\n{USAGE}"));
        }
    };
    // what has no window says what it does in a terminal
    if args.dedicated || args.master {
        terminal::open();
    }
    say!("openGMOD {}", env!("CARGO_PKG_VERSION"));
    if let Some(rate) = args.tick_rate {
        gmod_lua::timer::set_tick_rate(rate);
        say!("Tick rate: {rate} ({:.4} s a tick)", gmod_lua::timer::tick_interval());
    }
    if args.master {
        return match master(args.port.unwrap_or(gmod_net::MASTER_PORT)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    }
    let disabled = menu::disabled_file().map(|file| gmod_fs::workshop::read_disabled(&file)).unwrap_or_default();
    // to play, Steam: an account that owns the game. A dedicated server goes without
    let steam = if args.dedicated {
        None
    } else {
        let install = match Install::locate(args.gmod.as_deref()) {
            Ok(install) => install,
            Err(e) => return fail(&format!("Garry's Mod was not found: {e}")),
        };
        match gmod_steam::Steam::start(&install.root) {
            Ok(steam) => {
                say!("Steam: {} ({})", steam.name(), steam.steam_id());
                Some(Rc::new(steam))
            }
            Err(e) => return fail(&format!("openGMOD plays with a licensed Garry's Mod, through Steam: {e}.")),
        }
    };
    let setup = Setup {
        gmod: args.gmod.clone(),
        addons: args.addons,
        lua: args.lua,
        gamemode: args.gamemode.clone().unwrap_or_else(|| "sandbox".into()),
        disabled,
        max_players: args.max_players.unwrap_or(if args.dedicated { 16 } else { 1 }),
        port: args.port.unwrap_or(proto::DEFAULT_PORT),
        announce: args.announce.clone(),
        name: steam.as_ref().map(|steam| steam.name()).unwrap_or_else(|| "Player".to_string()),
        // a dedicated server has no player of its own
        identity: if args.dedicated { gmod_net::Identity::generate() } else { keys::player_identity() },
        subscribed: steam.as_ref().map(|steam| steam.subscribed(true).into_iter().collect()),
    };
    // a dedicated server: the only way without Steam
    let Some(steam) = steam else {
        let Some(map) = &args.map else { return fail(&format!("--dedicated needs a map\n\n{USAGE}")) };
        let hostname = args.hostname.clone().unwrap_or_else(|| "openGMOD".to_string());
        return match dedicated(&setup, map, &hostname) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        };
    };

    // the main menu, drawn by Chromium, on the game's files
    let (mut web, mut main_menu) = (None, None);
    match Install::locate(setup.gmod.as_deref()) {
        Ok(install) if args.html && args.lua => {
            say!("Garry's Mod: {}", install.root.display());
            let vfs = Arc::new(setup.mount(&install));
            match menu::start_web(vfs.clone()) {
                Ok(started) => {
                    let started = Rc::new(started);
                    let addons = menu::addons(&install, &setup.disabled, setup.subscribed.as_ref());
                    match menu::Menu::start(
                        vfs,
                        Some(menu::web_pages(started.clone())),
                        &setup.gamemode,
                        addons,
                        menu::games(&install),
                        !args.addons,
                    ) {
                        Ok(menu) => {
                            menu.runtime().set_steam(steam.clone());
                            main_menu = Some(menu);
                        }
                        Err(e) => say!("No main menu: {e}"),
                    }
                    web = Some(started);
                }
                Err(e) => say!("No Chromium, so no main menu and no HTML: {e}"),
            }
        }
        _ => {}
    }
    let map = match (&args.map, &main_menu) {
        (Some(map), _) => Some(map.clone()),
        _ if args.connect.is_some() => None,
        (None, None) => Some(DEFAULT_MAP.to_string()),
        (None, Some(_)) => None,
    };

    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(e) => return fail(&format!("window system: {e}")),
    };
    // draw as fast as the screen takes it
    event_loop.set_control_flow(ControlFlow::Poll);
    let now = Instant::now();
    let config = settings::Config::load(Install::locate(setup.gmod.as_deref()).ok().map(|install| install.game_dir()).as_deref());
    if let Some(menu) = &main_menu {
        menu.runtime().set_settings(config.values(), config.binds().to_vec());
        menu.runtime().set_convar("gmod_language", config.value("gmod_language"));
    }
    let mut app = App {
        config,
        world: None,
        collision: None,
        models: None,
        audio: None,
        light: None,
        lit: HashMap::new(),
        game: None,
        status: String::new(),
        settings: Settings::default(),
        spawn: None,
        walker: None,
        look: None,
        icons: std::collections::VecDeque::new(),
        name: "main menu".to_string(),
        camera: Camera::looking_at([0.0; 3], [1.0, 0.0, 0.0], FOV),
        gpu: None,
        error: None,
        keys: HashSet::new(),
        mouse: [false; 2],
        wheel: 0,
        mouse_moved: [0.0; 2],
        holding: false,
        captured: false,
        focused: true,
        ui_cursor: false,
        captured_before_ui: false,
        chat_esc: false,
        last_frame: now,
        frames: 0,
        counting_since: now,
        screenshot: false,
        setup,
        menu: main_menu,
        menu_open: false,
        console_menu: false,
        console_seen: 0,
        web: web.take(),
        pending_map: None,
        pending_connect: None,
        loading: None,
        quit: false,
        menu_started: now,
        menu_drawn: false,
        menu_late: false,
        frames_drawn: 0,
        steam,
    };
    if let Some(map) = map {
        app.enter_map(&map);
        if app.collision.is_none() && app.menu.is_none() {
            return fail(&format!("{map} could not be loaded"));
        }
    }
    if let Some(address) = &args.connect {
        app.connect(address);
        if app.collision.is_none() && app.menu.is_none() {
            return fail(&format!("{address}: could not join"));
        }
    }
    if let Err(e) = event_loop.run_app(&mut app) {
        return fail(&format!("window system: {e}"));
    }
    // a server here tells its clients it goes, a client its server that the player does
    app.leave_map();
    // the pages go before Chromium does
    let error = app.error.take();
    drop(app);
    match error {
        Some(e) => fail(&e),
        None => ExitCode::SUCCESS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Args>, String> {
        parse_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn arguments() {
        let args = parse(&[]).unwrap().unwrap();
        assert_eq!((args.map.as_deref(), args.addons, args.lua, args.html), (None, true, true, true));
        assert!(!parse(&["--no-lua"]).unwrap().unwrap().lua);
        assert!(!parse(&["--no-html"]).unwrap().unwrap().html);
        assert_eq!(parse(&["-tickrate", "33"]).unwrap().unwrap().tick_rate, Some(33.0));
        assert!(parse(&["--tickrate", "5"]).is_err());
        let args = parse(&["gm_flatgrass", "--gmod", "D:/GarrysMod", "--no-addons"]).unwrap().unwrap();
        assert_eq!(
            (args.map.as_deref(), args.gmod.as_deref(), args.addons),
            (Some("gm_flatgrass"), Some(Path::new("D:/GarrysMod")), false)
        );
        assert_eq!(parse(&["--map", "x"]).unwrap().unwrap().map.as_deref(), Some("x"));
        assert!(parse(&["-h"]).unwrap().is_none());
        assert!(parse(&["a", "b"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
    }

    #[test]
    fn keys_have_the_games_numbers() {
        assert_eq!(gmod_key(KeyCode::Digit0), Some(1));
        assert_eq!(gmod_key(KeyCode::KeyA), Some(11));
        assert_eq!(gmod_key(KeyCode::KeyQ), Some(27));
        assert_eq!(gmod_key(KeyCode::KeyZ), Some(36));
        assert_eq!(gmod_key(KeyCode::Numpad9), Some(46));
        assert_eq!(gmod_key(KeyCode::Enter), Some(64));
        assert_eq!(gmod_key(KeyCode::ArrowRight), Some(91));
        assert_eq!(gmod_key(KeyCode::F12), Some(103));
        assert_eq!(gmod_key(KeyCode::MediaPlayPause), None);
    }

    #[test]
    fn futures_run_to_their_end() {
        assert_eq!(block_on(async { 6 * 7 }), 42);
    }

    #[test]
    fn direct3d_first_but_a_real_gpu_before_a_software_one() {
        use wgpu::{Backend, DeviceType};
        let mut adapters = [
            (DeviceType::Cpu, Backend::Dx12),
            (DeviceType::IntegratedGpu, Backend::Vulkan),
            (DeviceType::DiscreteGpu, Backend::Vulkan),
            (DeviceType::IntegratedGpu, Backend::Dx12),
            (DeviceType::DiscreteGpu, Backend::Dx12),
        ];
        adapters.sort_by_key(|&(kind, backend)| adapter_rank(kind, backend, None));
        assert_eq!(
            adapters,
            [
                (DeviceType::DiscreteGpu, Backend::Dx12),
                (DeviceType::DiscreteGpu, Backend::Vulkan),
                (DeviceType::IntegratedGpu, Backend::Dx12),
                (DeviceType::IntegratedGpu, Backend::Vulkan),
                (DeviceType::Cpu, Backend::Dx12),
            ]
        );
    }

    #[test]
    fn the_chosen_api_first_on_any_gpu() {
        use wgpu::{Backend, DeviceType};
        let mut adapters = [
            (DeviceType::DiscreteGpu, Backend::Dx12),
            (DeviceType::IntegratedGpu, Backend::Vulkan),
            (DeviceType::DiscreteGpu, Backend::Vulkan),
        ];
        adapters.sort_by_key(|&(kind, backend)| adapter_rank(kind, backend, Some(Backend::Vulkan)));
        assert_eq!(adapters[0], (DeviceType::DiscreteGpu, Backend::Vulkan));
        assert_eq!(adapters[2], (DeviceType::DiscreteGpu, Backend::Dx12), "and the others after, should it not be there");
    }
}
