//! `opengmod`: for now, a map from your Garry's Mod on the screen to fly around in - the world,
//! brush entities and static props with their textures and light, and the sky box. Drawn with
//! Direct3D 12 on Windows (Vulkan when that is missing).

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use gmod_fs::UserDir;
use gmod_fs::install::Install;
use gmod_fs::mount::{MountOptions, mount_install};
use gmod_render::block_on;
use gmod_world::World;
use gmod_world::gpu::Camera;

const USAGE: &str = "\
opengmod - a Garry's Mod map from your installation, to fly around in

USAGE:
    opengmod [OPTIONS] [MAP]

    MAP                  gm_construct (the default), another map of the game, or a .bsp file

OPTIONS:
    --gmod <DIR>         the Garry's Mod folder (default: GMOD_ROOT, then Steam's libraries)
    --no-addons          the game only: no workshop items, no addon files
    -h, --help           this text

CONTROLS:
    click the window to look around with the mouse, Esc to let the mouse go
    W A S D to fly, Space and Ctrl up and down, Shift faster
    F11 full screen, F12 a screenshot (saved in ~/.opengmod/screenshots)";

/// Degrees the view turns per pixel the mouse moves.
const MOUSE_DEGREES: f32 = 0.12;
/// Units a second; Shift makes it four times as much.
const SPEED: f32 = 400.0;
/// The game's field of view, across a 4:3 picture.
const FOV: f32 = 75.0;

struct Args {
    map: String,
    gmod: Option<PathBuf>,
    addons: bool,
}

fn parse_args(mut raw: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut args = Args { map: "gm_construct".into(), gmod: None, addons: true };
    let mut map = None;
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--gmod" => args.gmod = Some(raw.next().ok_or("--gmod needs a folder")?.into()),
            "--map" => map = Some(raw.next().ok_or("--map needs a map")?),
            "--no-addons" => args.addons = false,
            "-h" | "--help" => return Ok(None),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if map.is_none() => map = Some(arg),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    if let Some(map) = map {
        args.map = map;
    }
    Ok(Some(args))
}

/// Reads the map and everything it needs from the game.
fn load(args: &Args) -> Result<(World, Camera, String), String> {
    let started = Instant::now();
    let on_disk = Path::new(&args.map).is_file();
    let install = Install::locate(args.gmod.as_deref());
    let mut vfs = match &install {
        Ok(install) => {
            println!("Garry's Mod: {}", install.root.display());
            let unpack_dir = UserDir::default_root().map(|root| root.join("cache").join("workshop"));
            let options = MountOptions { addons: args.addons, unpack_dir, ..MountOptions::default() };
            mount_install(install, &options).0
        }
        // a map file can be shown without the game, only not with its materials
        Err(e) if on_disk => {
            println!("{e}\nThe map is drawn without the game's materials.");
            gmod_fs::Vfs::new()
        }
        Err(e) => return Err(e.to_string()),
    };
    let path = if on_disk {
        args.map.clone()
    } else {
        let name = args.map.trim_end_matches(".bsp");
        if name.contains(['/', '\\']) { format!("{name}.bsp") } else { format!("maps/{name}.bsp") }
    };
    let bytes = if on_disk { std::fs::read(&path) } else { vfs.read(&path, "GAME") }.map_err(|e| format!("{path}: {e}"))?;
    let bsp = gmod_bsp::Bsp::parse(PathBuf::from(&path), Arc::from(bytes)).map_err(|e| e.to_string())?;
    gmod_world::mount_pakfile(&mut vfs, &bsp).map_err(|e| format!("{path}: pakfile: {e}"))?;
    let world = gmod_world::load(&vfs, &bsp);
    println!("{path}: {} triangles, {} textures, in {:.1} s", world.triangles(), world.textures.len(), started.elapsed().as_secs_f64());
    for (what, names) in [("materials", &world.missing_materials), ("textures", &world.missing_textures), ("models", &world.missing_models)]
    {
        if !names.is_empty() {
            let shown: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
            let more = if names.len() > 3 { format!(" and {} more", names.len() - 3) } else { String::new() };
            println!("Missing {what} ({}): {}{more}", names.len(), shown.join(", "));
        }
    }
    let (eye, target) = gmod_world::default_view(&bsp);
    let name = Path::new(&path).file_stem().map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned());
    Ok((world, Camera::looking_at(eye, target, FOV), name))
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
    renderer: gmod_render::Renderer,
}

impl Gpu {
    fn new(event_loop: &ActiveEventLoop, title: &str, world: &World) -> Result<Gpu, String> {
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
        let started = Instant::now();
        let renderer = gmod_render::Renderer::new(&device, &queue, view_format, world);
        println!("Uploaded to the GPU in {:.1} s", started.elapsed().as_secs_f64());
        if !renderer.failed_textures.is_empty() {
            println!("Textures that could not be shown: {}", renderer.failed_textures.len());
        }
        Ok(Gpu { window, instance, surface, device, queue, config, view_format, renderer })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Draws one more frame into a texture of its own and saves it as a PNG in
    /// `~/.opengmod/screenshots`.
    fn screenshot(&mut self, camera: &Camera, name: &str) -> Result<PathBuf, String> {
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
        self.renderer.render(&self.device, &self.queue, &mut encoder, &view, (width, height), camera);
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

    fn draw(&mut self, camera: &Camera) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                drop(frame);
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
                    self.surface = surface;
                    self.surface.configure(&self.device, &self.config);
                }
                return;
            }
            // hidden, or not ready yet: the next frame
            _ => return,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor { format: Some(self.view_format), ..Default::default() });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        self.renderer.render(&self.device, &self.queue, &mut encoder, &view, (self.config.width, self.config.height), camera);
        self.queue.submit([encoder.finish()]);
        self.window.pre_present_notify();
        self.queue.present(frame);
    }
}

struct App {
    world: World,
    name: String,
    camera: Camera,
    gpu: Option<Gpu>,
    error: Option<String>,
    keys: HashSet<KeyCode>,
    captured: bool,
    last_frame: Instant,
    frames: u32,
    counting_since: Instant,
    /// F12 was pressed: save the next frame.
    screenshot: bool,
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
    }

    /// Moves the camera by the keys held, for the time since the last frame.
    fn fly(&mut self) {
        let now = Instant::now();
        let seconds = (now - self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;
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

    fn count_frame(&mut self) {
        self.frames += 1;
        let elapsed = self.counting_since.elapsed().as_secs_f32();
        if elapsed >= 1.0 {
            if let Some(gpu) = &self.gpu {
                let [x, y, z] = self.camera.eye.map(|v| v.round() as i32);
                gpu.window.set_title(&format!("openGMOD - {} - {:.0} fps - {x} {y} {z}", self.name, self.frames as f32 / elapsed));
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
        match Gpu::new(event_loop, &format!("openGMOD - {}", self.name), &self.world) {
            Ok(gpu) => {
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
            WindowEvent::Focused(false) => {
                self.keys.clear();
                self.capture(false);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => self.capture(true),
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(key), state, repeat, .. }, .. } => {
                if state == ElementState::Pressed {
                    match key {
                        KeyCode::Escape => self.capture(false),
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
                self.fly();
                if let Some(gpu) = &mut self.gpu {
                    gpu.draw(&self.camera);
                    if std::mem::take(&mut self.screenshot) {
                        match gpu.screenshot(&self.camera, &self.name) {
                            Ok(file) => println!("Screenshot: {}", file.display()),
                            Err(e) => println!("No screenshot: {e}"),
                        }
                    }
                    gpu.window.request_redraw();
                }
                self.count_frame();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _device: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
            && self.captured
        {
            self.camera.yaw -= dx as f32 * MOUSE_DEGREES;
            self.camera.pitch = (self.camera.pitch + dy as f32 * MOUSE_DEGREES).clamp(-89.0, 89.0);
        }
    }
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
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => return fail(&format!("{e}\n\n{USAGE}")),
    };
    println!("openGMOD {}", env!("CARGO_PKG_VERSION"));
    let (world, camera, name) = match load(&args) {
        Ok(loaded) => loaded,
        Err(e) => return fail(&e),
    };
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(e) => return fail(&format!("window system: {e}")),
    };
    // draw as fast as the screen takes it
    event_loop.set_control_flow(ControlFlow::Poll);
    let now = Instant::now();
    let mut app = App {
        world,
        name,
        camera,
        gpu: None,
        error: None,
        keys: HashSet::new(),
        captured: false,
        last_frame: now,
        frames: 0,
        counting_since: now,
        screenshot: false,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        return fail(&format!("window system: {e}"));
    }
    match app.error {
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
        assert_eq!((args.map.as_str(), args.addons), ("gm_construct", true));
        let args = parse(&["gm_flatgrass", "--gmod", "D:/GarrysMod", "--no-addons"]).unwrap().unwrap();
        assert_eq!((args.map.as_str(), args.gmod.as_deref(), args.addons), ("gm_flatgrass", Some(Path::new("D:/GarrysMod")), false));
        assert_eq!(parse(&["--map", "x"]).unwrap().unwrap().map, "x");
        assert!(parse(&["-h"]).unwrap().is_none());
        assert!(parse(&["a", "b"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
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
