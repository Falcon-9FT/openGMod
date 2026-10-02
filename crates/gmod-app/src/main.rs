//! `opengmod`: Garry's Mod's main menu ([`menu`], drawn by Chromium), and the maps it starts on
//! the screen - the world, brush entities and static props with their textures and light, and
//! the sky box - with the game running in them: the Lua of the server, the gamemode and the
//! addons ([`game`]), and the player of a single-player game walking through it as the game
//! moves them. Drawn with Direct3D 12 on Windows (Vulkan when that is missing). Without the
//! game's Lua (`--no-lua`, or a map file without the game) the player only walks
//! ([`gmod_movement`]).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

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

mod audio;
mod game;
mod menu;
mod walk;

use game::{Buttons, Game, Models};
use walk::{Keys, Walker};

const USAGE: &str = "\
opengmod - Garry's Mod's menu and maps from your installation, with the game's Lua and your addons

USAGE:
    opengmod [OPTIONS] [MAP]

    MAP                  a map to start in: one of the game (gm_construct), or a .bsp file;
                         without one, the main menu

OPTIONS:
    --gmod <DIR>         the Garry's Mod folder (default: GMOD_ROOT, then Steam's libraries)
    --no-addons          the game only: no workshop items, no addon files
    --no-lua             no Lua: only walk through the map
    --no-html            no Chromium: no main menu (gm_construct starts) and no HTML panels
    -h, --help           this text

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

    Lines typed into this terminal are the game's console: give weapon_crowbar,
    gm_spawn models/props_c17/oildrum001.mdl, the commands of the addons.";

/// Degrees the view turns per pixel the mouse moves.
const MOUSE_DEGREES: f32 = 0.12;
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
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args { map: None, gmod: None, addons: true, lua: true, html: true };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--gmod" => args.gmod = Some(raw.next().ok_or("--gmod needs a folder")?.into()),
            "--map" => args.map = Some(raw.next().ok_or("--map needs a map")?),
            "--no-addons" => args.addons = false,
            "--no-lua" => args.lua = false,
            "--no-html" => args.html = false,
            "-h" | "--help" => return Ok(None),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if args.map.is_none() => args.map = Some(arg),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    Ok(Some(args))
}

/// How games are started: from where, with what.
struct Setup {
    gmod: Option<PathBuf>,
    addons: bool,
    lua: bool,
    gamemode: String,
    /// Addons switched off in the menu.
    disabled: HashSet<String>,
}

impl Setup {
    /// The game's files as Garry's Mod mounts them, the addons switched off left out.
    fn mount(&self, install: &Install) -> Vfs {
        let unpack_dir = UserDir::default_root().map(|root| root.join("cache").join("workshop"));
        let options = MountOptions { addons: self.addons, unpack_dir, disabled: self.disabled.clone(), ..MountOptions::default() };
        mount_install(install, &options).0
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
}

/// Reads the map and everything it needs from the game.
fn load(setup: &Setup, map: &str) -> Result<Loaded, String> {
    let started = Instant::now();
    let on_disk = Path::new(map).is_file();
    let install = Install::locate(setup.gmod.as_deref());
    let mut vfs = match &install {
        Ok(install) => setup.mount(install),
        // a map file can be shown without the game, only not with its materials
        Err(e) if on_disk => {
            println!("{e}\nThe map is drawn without the game's materials.");
            gmod_fs::Vfs::new()
        }
        Err(e) => return Err(e.to_string()),
    };
    let path = if on_disk {
        map.to_string()
    } else {
        let name = map.trim_end_matches(".bsp");
        if name.contains(['/', '\\']) { format!("{name}.bsp") } else { format!("maps/{name}.bsp") }
    };
    let bytes = if on_disk { std::fs::read(&path) } else { vfs.read(&path, "GAME") }.map_err(|e| format!("{path}: {e}"))?;
    let bsp = gmod_bsp::Bsp::parse(PathBuf::from(&path), Arc::from(bytes)).map_err(|e| e.to_string())?;
    gmod_world::mount_pakfile(&mut vfs, &bsp).map_err(|e| format!("{path}: pakfile: {e}"))?;
    let world = gmod_world::load(&vfs, &bsp);
    let (collision, unread_physics) = gmod_world::collision(&vfs, &bsp);
    println!("{path}: {} triangles, {} textures, in {:.1} s", world.triangles(), world.textures.len(), started.elapsed().as_secs_f64());
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
            println!("{what} ({}): {}{more}", names.len(), shown.join(", "));
        }
    }
    let (eye, target) = gmod_world::default_view(&bsp);
    let name = Path::new(&path).file_stem().map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned());
    let collision = Arc::new(collision);
    let lua = (setup.lua && install.is_ok()).then(|| {
        let map = gmod_lua::Map::from_bsp(&name, &bsp, collision.clone());
        (Arc::new(vfs), map)
    });
    let light = gmod_world::light::MapLight::new(&bsp);
    Ok(Loaded { world, collision, light, lua, camera: Camera::looking_at(eye, target, FOV), spawn: gmod_world::spawn_point(&bsp), name })
}

/// How much a graphics adapter is wanted, lowest first: a real GPU before a software one (WARP,
/// llvmpipe), a discrete one before an integrated one, and of the same kind Direct3D 12 before
/// Vulkan.
fn adapter_rank(kind: wgpu::DeviceType, backend: wgpu::Backend) -> (u8, bool) {
    let kind = match kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::Other => 2,
        wgpu::DeviceType::VirtualGpu => 3,
        wgpu::DeviceType::Cpu => 4,
    };
    (kind, backend != wgpu::Backend::Dx12)
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
    /// The client's 2D over the world: the HUD, the menus.
    overlay: gmod_render::Overlay,
    /// The main menu's, over everything.
    menu_overlay: gmod_render::Overlay,
}

impl Gpu {
    fn new(event_loop: &ActiveEventLoop, title: &str) -> Result<Gpu, String> {
        let attributes = Window::default_attributes().with_title(title).with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = Arc::new(event_loop.create_window(attributes).map_err(|e| format!("window: {e}"))?);
        let mut descriptor = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle()));
        let backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
        descriptor.backends = backends;
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance.create_surface(window.clone()).map_err(|e| format!("surface: {e}"))?;
        // chosen here and not by request_adapter, which takes Vulkan first when both drive a GPU
        let adapter = block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .filter(|adapter| adapter.is_surface_supported(&surface))
            .min_by_key(|adapter| {
                let info = adapter.get_info();
                adapter_rank(info.device_type, info.backend)
            })
            .ok_or("no graphics adapter with Direct3D 12 or Vulkan")?;
        let info = adapter.get_info();
        println!("GPU: {} ({:?})", info.name, info.backend);
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
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![view_format],
        };
        surface.configure(&device, &config);
        let overlay = gmod_render::Overlay::new(&device, &queue, view_format);
        let menu_overlay = gmod_render::Overlay::new(&device, &queue, view_format);
        Ok(Gpu { window, instance, surface, device, queue, config, view_format, renderer: None, overlay, menu_overlay })
    }

    /// The map to draw from now on, or none.
    fn set_world(&mut self, world: Option<&World>) {
        self.renderer = None;
        // the client's images were of the game left
        self.overlay = gmod_render::Overlay::new(&self.device, &self.queue, self.view_format);
        let Some(world) = world else { return };
        let started = Instant::now();
        let renderer = gmod_render::Renderer::new(&self.device, &self.queue, self.view_format, world);
        println!("Uploaded to the GPU in {:.1} s", started.elapsed().as_secs_f64());
        if !renderer.failed_textures.is_empty() {
            println!("Textures that could not be shown: {}", renderer.failed_textures.len());
        }
        self.renderer = Some(renderer);
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// The map, or black where there is none.
    fn draw_3d(&mut self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, size: (u32, u32), camera: &Camera) {
        match &mut self.renderer {
            Some(renderer) => renderer.render(&self.device, &self.queue, encoder, view, size, camera),
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
            draw_2d(&self.device, &self.queue, &mut self.overlay, encoder, view, size, client);
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
        self.draw_3d(&mut encoder, &view, (width, height), camera);
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
        self.draw_3d(&mut encoder, &view, size, camera);
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
/// new images go to the GPU first.
fn draw_2d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    overlay: &mut gmod_render::Overlay,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    size: (u32, u32),
    runtime: &gmod_lua::Runtime,
) {
    for (id, picture) in runtime.take_page_pictures() {
        overlay.update_image(device, queue, id, picture.width, picture.height, &picture.pixels);
    }
    let missing = runtime.with_canvas(|canvas, fonts| {
        overlay.update_glyphs(device, queue, fonts);
        overlay.missing_images(canvas)
    });
    for id in missing {
        let pixels = runtime.image(id).and_then(|(_, pixels)| pixels);
        overlay.set_image(device, queue, id, pixels.as_ref().map(|p| (p.width, p.height, p.pixels.as_slice())));
    }
    runtime.with_canvas(|canvas, _| overlay.render(device, queue, encoder, view, size, canvas));
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
    /// Chromium, for the menu and the game's HTML panels.
    web: Option<Rc<gmod_web::Web>>,
    /// A map the menu asked for, loaded on the next frame (after one that says so).
    pending_map: Option<String>,
    /// The menu said to quit.
    quit: bool,
    /// When the menu started, whether Chromium drew it, and whether it was said that it did not.
    menu_started: Instant,
    menu_drawn: bool,
    menu_late: bool,
    /// Frames drawn since the start.
    frames_drawn: u64,
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
            println!("No room to stand here");
        }
    }

    /// The keys that move a walking player.
    fn movement_keys(&self) -> Keys {
        let held = |key: KeyCode| self.keys.contains(&key);
        Keys {
            forward: held(KeyCode::KeyW),
            back: held(KeyCode::KeyS),
            left: held(KeyCode::KeyA),
            right: held(KeyCode::KeyD),
            jump: held(KeyCode::Space),
            duck: held(KeyCode::ControlLeft) || held(KeyCode::ControlRight),
            run: held(KeyCode::ShiftLeft) || held(KeyCode::ShiftRight),
        }
    }

    /// The buttons held besides the walking keys.
    fn buttons(&self) -> Buttons {
        let held = |key: KeyCode| self.keys.contains(&key);
        Buttons {
            attack: self.mouse[0],
            attack2: self.mouse[1],
            reload: held(KeyCode::KeyR),
            using: held(KeyCode::KeyE),
            walk: held(KeyCode::AltLeft) || held(KeyCode::AltRight),
            ..Buttons::default()
        }
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

    /// Shows the menu over the game (Esc), or goes back to the game.
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

    /// Starts a game on a map, leaving the one there was: the menu asked, or the command line.
    fn enter_map(&mut self, map: &str) {
        self.leave_map();
        println!("Loading {map}");
        let loaded = match load(&self.setup, map) {
            Ok(loaded) => loaded,
            Err(e) => {
                eprintln!("{e}");
                return;
            }
        };
        let pages = self.web.clone().map(menu::web_pages);
        self.game = loaded.lua.as_ref().and_then(|(vfs, map)| {
            let started = Instant::now();
            match Game::start(vfs.clone(), map, &self.setup.gamemode, pages) {
                Ok(game) => {
                    println!("The game started in {:.1} s", started.elapsed().as_secs_f64());
                    Some(game)
                }
                Err(e) => {
                    println!("{e}\nThe map is walked without the game's Lua.");
                    None
                }
            }
        });
        self.models = self.game.as_ref().and(loaded.lua.as_ref()).map(|(vfs, _)| Models::new(vfs.clone()));
        self.audio = self.game.as_ref().and(loaded.lua.as_ref()).map(|(vfs, _)| audio::Audio::new(vfs.clone()));
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
                println!("No player start with room to stand: flying (V walks from where you are)");
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
        self.game = None;
        self.models = None;
        self.audio = None;
        self.collision = None;
        self.light = None;
        self.lit.clear();
        self.walker = None;
        self.ui_cursor = false;
        self.name = "main menu".to_string();
        if let Some(gpu) = &mut self.gpu {
            gpu.set_world(None);
        }
        self.capture(false);
        if let Some(menu) = &self.menu {
            menu.runtime().set_in_game(false);
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
            println!("Chromium drew the main menu in {seconds:.1} s");
        } else if seconds > 10.0 && !self.menu_late {
            self.menu_late = true;
            println!(
                "Chromium has not drawn the main menu in 10 s ({pages} pages, {} frames drawn); \
                 its log is in ~/.opengmod/logs/chromium.log. opengmod --no-html goes without it.",
                self.frames_drawn
            );
        }
    }

    /// A frame of the main menu: Chromium's work, the menu's Lua when it shows, and what it
    /// asked of the engine.
    fn menu_frame(&mut self, seconds: f32) {
        if let Some(web) = &self.web {
            web.pump();
        }
        self.report_menu();
        let Some(menu) = &self.menu else { return };
        if self.menu_shown()
            && let Some(gpu) = &self.gpu
        {
            menu.frame(gpu.config.width, gpu.config.height, f64::from(seconds));
        }
        if self.game.is_none() {
            for line in game::typed_lines() {
                menu.console(&line);
            }
        }
        if let Some(addons) = menu.runtime().take_addon_changes() {
            self.setup.disabled = addons.iter().filter(|a| !a.mounted).map(menu::addon_key).collect();
            let on = addons.iter().filter(|a| a.mounted).count();
            println!("Addons: {on} on, {} off - from the next game on", addons.len() - on);
            if let Some(file) = menu::disabled_file()
                && let Err(e) = gmod_fs::workshop::write_disabled(&file, &self.setup.disabled)
            {
                eprintln!("{}: {e}", file.display());
            }
        }
        for line in menu.take_commands() {
            let words: Vec<&str> = line.split_whitespace().map(|w| w.trim_matches('"')).collect();
            match words.as_slice() {
                ["map", map, ..] => self.pending_map = Some(map.to_string()),
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
                [
                    "progress_enable" | "maxplayers" | "sv_cheats" | "hostname" | "p2p_enabled" | "p2p_friendsonly" | "sv_lan"
                    | "commentary",
                    ..,
                ] => {}
                _ => match &self.game {
                    Some(game) => game.command(&line),
                    None => println!("Unknown command \"{line}\""),
                },
            }
            if self.menu.is_none() {
                break;
            }
        }
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

    /// A frame of the game: the player's command, the ticks, and the eyes to see from.
    fn play(&mut self, seconds: f32) {
        let (keys, mut buttons) = (self.movement_keys(), self.buttons());
        buttons.wheel = std::mem::take(&mut self.wheel);
        buttons.mouse = self.mouse_moved.map(|counts| counts.trunc() as i32);
        self.mouse_moved = self.mouse_moved.map(f64::fract);
        let Some(game) = &mut self.game else { return };
        let Some(shown) = game.frame(&keys, &buttons, self.camera.pitch, self.camera.yaw, f64::from(seconds)) else { return };
        self.holding = shown.view.holding;
        self.camera.eye = shown.eye;
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
        let Some(game) = &mut self.game else { return false };
        let slot = [KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4, KeyCode::Digit5, KeyCode::Digit6]
            .iter()
            .position(|&k| k == key);
        match (key, slot) {
            (_, Some(slot)) => game.command(&format!("slot{}", slot + 1)),
            (KeyCode::KeyV, _) => game.command("noclip"),
            (KeyCode::KeyZ, _) => game.command("gmod_undo"),
            (KeyCode::KeyF, _) => game.flashlight(),
            _ => return false,
        }
        true
    }

    /// Flies the camera by the keys held, through walls.
    fn fly(&mut self, seconds: f32) {
        let held = |key: KeyCode| self.keys.contains(&key);
        let forward = self.camera.forward();
        let right = self.camera.right();
        let mut step = [0.0f32; 3];
        let mut push = |direction: [f32; 3], amount: f32| {
            for (s, d) in step.iter_mut().zip(direction) {
                *s += d * amount;
            }
        };
        push(forward, f32::from(held(KeyCode::KeyW)) - f32::from(held(KeyCode::KeyS)));
        push(right, f32::from(held(KeyCode::KeyD)) - f32::from(held(KeyCode::KeyA)));
        push([0.0, 0.0, 1.0], f32::from(held(KeyCode::Space)) - f32::from(held(KeyCode::ControlLeft) || held(KeyCode::ControlRight)));
        let fast = if held(KeyCode::ShiftLeft) || held(KeyCode::ShiftRight) { 4.0 } else { 1.0 };
        for (eye, s) in self.camera.eye.iter_mut().zip(step) {
            *eye += s * SPEED * fast * seconds;
        }
    }

    /// Gives the renderer the models of the game's entities and where they are now.
    fn show_entities(&mut self) {
        let (Some(game), Some(models), Some(gpu)) = (&self.game, &mut self.models, &mut self.gpu) else { return };
        let Some(renderer) = &mut gpu.renderer else { return };
        let mut placed = models.place(&game.drawn(), |model| renderer.add_model(&gpu.device, &gpu.queue, model));
        // the light where each stands, worked out once for every 8 units
        if self.lit.len() > 8192 {
            self.lit.clear();
        }
        let (Some(light), Some(collision)) = (&self.light, &self.collision) else { return };
        for p in &mut placed {
            let key = p.origin.map(|v| (v / 8.0).round() as i32);
            p.light = self.lit.entry(key).or_insert_with(|| light.at(collision, p.origin)).clone();
        }
        renderer.set_entities(placed);
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
        match Gpu::new(event_loop, &format!("openGMOD - {}", self.name)) {
            Ok(mut gpu) => {
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
                        game.command(if up { "invprev" } else { "invnext" });
                    }
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(key), state, repeat, text, .. }, .. } => {
                let pressed = state == ElementState::Pressed;
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
                        KeyCode::KeyV if !repeat => self.toggle_noclip(),
                        KeyCode::F12 if !repeat => self.screenshot = true,
                        KeyCode::F11 if !repeat => {
                            if let Some(gpu) = &self.gpu {
                                let full = gpu.window.fullscreen().is_some();
                                gpu.window.set_fullscreen(if full { None } else { Some(Fullscreen::Borderless(None)) });
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
                    self.enter_map(&map);
                }
                let now = Instant::now();
                let seconds = (now - self.last_frame).as_secs_f32().min(0.25);
                self.last_frame = now;
                // the game waits while the menu shows over it, as a single-player game does
                if !self.menu_shown() {
                    self.advance(seconds);
                }
                self.show_entities();
                if let (Some(game), Some(gpu)) = (&self.game, &self.gpu) {
                    game.paint(gpu.config.width, gpu.config.height, f64::from(seconds));
                }
                self.menu_frame(seconds);
                if !self.menu_shown() {
                    self.follow_ui_cursor();
                }
                let menu = self.menu.as_ref().filter(|_| self.menu_shown()).map(menu::Menu::runtime);
                if let Some(gpu) = &mut self.gpu {
                    let client = self.game.as_ref().map(Game::client);
                    if gpu.draw(&self.camera, client, menu) {
                        self.frames_drawn += 1;
                    }
                    if std::mem::take(&mut self.screenshot) {
                        match gpu.screenshot(&self.camera, client, menu, &self.name) {
                            Ok(file) => println!("Screenshot: {}", file.display()),
                            Err(e) => println!("No screenshot: {e}"),
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
            if !(self.holding && self.keys.contains(&KeyCode::KeyE)) {
                self.camera.yaw -= dx as f32 * MOUSE_DEGREES;
                self.camera.pitch = (self.camera.pitch + dy as f32 * MOUSE_DEGREES).clamp(-89.0, 89.0);
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
    eprintln!("{message}");
    if started_from_explorer() {
        eprintln!("\nPress Enter to close.");
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
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => return fail(&format!("{e}\n\n{USAGE}")),
    };
    println!("openGMOD {}", env!("CARGO_PKG_VERSION"));
    let disabled = menu::disabled_file().map(|file| gmod_fs::workshop::read_disabled(&file)).unwrap_or_default();
    let setup = Setup { gmod: args.gmod.clone(), addons: args.addons, lua: args.lua, gamemode: "sandbox".into(), disabled };

    // the main menu, drawn by Chromium, on the game's files
    let (mut web, mut main_menu) = (None, None);
    match Install::locate(setup.gmod.as_deref()) {
        Ok(install) if args.html && args.lua => {
            println!("Garry's Mod: {}", install.root.display());
            let vfs = Arc::new(setup.mount(&install));
            match menu::start_web(vfs.clone()) {
                Ok(started) => {
                    let started = Rc::new(started);
                    let addons = menu::addons(&install, &setup.disabled);
                    match menu::Menu::start(
                        vfs,
                        Some(menu::web_pages(started.clone())),
                        &setup.gamemode,
                        addons,
                        menu::games(&install),
                        !args.addons,
                    ) {
                        Ok(menu) => main_menu = Some(menu),
                        Err(e) => println!("No main menu: {e}"),
                    }
                    web = Some(started);
                }
                Err(e) => println!("No Chromium, so no main menu and no HTML: {e}"),
            }
        }
        _ => {}
    }
    let map = match (&args.map, &main_menu) {
        (Some(map), _) => Some(map.clone()),
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
    let mut app = App {
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
        last_frame: now,
        frames: 0,
        counting_since: now,
        screenshot: false,
        setup,
        menu: main_menu,
        menu_open: false,
        web: web.take(),
        pending_map: None,
        quit: false,
        menu_started: now,
        menu_drawn: false,
        menu_late: false,
        frames_drawn: 0,
    };
    if let Some(map) = map {
        app.enter_map(&map);
        if app.collision.is_none() && app.menu.is_none() {
            return fail(&format!("{map} could not be loaded"));
        }
    }
    if let Err(e) = event_loop.run_app(&mut app) {
        return fail(&format!("window system: {e}"));
    }
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
        adapters.sort_by_key(|&(kind, backend)| adapter_rank(kind, backend));
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
}
