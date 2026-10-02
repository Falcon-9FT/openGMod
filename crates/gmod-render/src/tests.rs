use std::sync::Arc;

use super::*;

fn validate(name: &str, source: &str) {
    let module = naga::front::wgsl::parse_str(source).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(source)));
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
}

#[test]
fn shaders_are_valid() {
    validate("world.wgsl", include_str!("world.wgsl"));
    validate("sky.wgsl", include_str!("sky.wgsl"));
    validate("ui.wgsl", include_str!("ui.wgsl"));
}

/// A GPU for the test: a real one, else a software one (WARP on Windows). Most machines that
/// run tests without a screen have neither on Linux; then the test is skipped.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    for fallback in [false, true] {
        let options = wgpu::RequestAdapterOptions { force_fallback_adapter: fallback, ..Default::default() };
        let Ok(adapter) = block_on(instance.request_adapter(&options)) else { continue };
        let features = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
        let descriptor = wgpu::DeviceDescriptor { required_features: features, ..Default::default() };
        if let Ok(pair) = block_on(adapter.request_device(&descriptor)) {
            println!("GPU: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
            return Some(pair);
        }
    }
    None
}

/// The device, or none when there is no GPU here and the test is skipped.
fn device_or_skip() -> Option<(wgpu::Device, wgpu::Queue)> {
    let pair = device();
    if pair.is_none() {
        // CI sets this on Windows, which always has WARP: there a skip would hide a broken renderer
        let required = std::env::var_os("OPENGMOD_REQUIRE_GPU").is_some_and(|v| !v.is_empty());
        assert!(!required, "no GPU, not even WARP, and OPENGMOD_REQUIRE_GPU is set");
        println!("no GPU here, not even a software one: skipped");
    }
    pair
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn test_map() -> World {
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(gmod_bsp::testmap::bytes())).unwrap();
    gmod_world::load(&gmod_fs::Vfs::new(), &bsp)
}

fn red(p: &[u8]) -> bool {
    p[0] > 200 && p[1] < 40 && p[2] < 40
}

/// A pixel of a picture: RGBA.
fn at(pixels: &[u8], x: usize, y: usize) -> &[u8] {
    &pixels[y * 256 + x * 4..y * 256 + x * 4 + 4]
}

/// Draws `world` from `camera` into a 64 x 64 picture and reads it back, RGBA.
fn draw(device: &wgpu::Device, queue: &wgpu::Queue, world: &World, camera: &Camera) -> Vec<u8> {
    picture(device, queue, &mut Renderer::new(device, queue, FORMAT, world), camera)
}

/// Draws what `renderer` has from `camera` into a 64 x 64 picture, rows of 256 bytes.
fn picture(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &mut Renderer, camera: &Camera) -> Vec<u8> {
    read_back(device, queue, |encoder, view| renderer.render(device, queue, encoder, view, (64, 64), camera))
}

/// A 64 x 64 picture of what `draw` draws, rows of 256 bytes.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, draw: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView)) -> Vec<u8> {
    let (width, height) = (64u32, 64u32);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("picture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("test") });
    draw(&mut encoder, &view);
    // rows of 256 bytes: what copies out of a texture need
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(256 * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(height) },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    readback.map_async(wgpu::MapMode::Read, .., |result| result.expect("the picture can be read back"));
    device.poll(wgpu::PollType::wait_indefinitely()).expect("the GPU finishes");
    readback.slice(..).get_mapped_range().expect("mapped").to_vec()
}

#[test]
fn draws_the_test_map() {
    let Some((device, queue)) = device_or_skip() else { return };
    // without the game, every material is missing: purple and black squares, lit by the light
    // maps - the floor's is red
    let world = test_map();

    // from above, looking down at the floor: red (and black) in the middle, the clear colour
    // around it
    let above = draw(&device, &queue, &world, &Camera::looking_at([64.0, 64.0, 200.0], [64.0, 64.1, 0.0], 75.0));
    let reds = (0..64 * 64).filter(|i| red(at(&above, i % 64, i / 64))).count();
    assert!(reds > 64 * 64 / 10, "{reds} red pixels");
    let corner = at(&above, 0, 0);
    assert!(corner[2] > corner[0] && corner[1] > corner[0], "{corner:?}");

    // from below the floor is turned away and not drawn: nothing red
    let below = draw(&device, &queue, &world, &Camera::looking_at([64.0, 64.0, -200.0], [64.0, 64.1, 0.0], 75.0));
    let reds = (0..64 * 64).filter(|i| red(at(&below, i % 64, i / 64))).count();
    assert_eq!(reds, 0, "the back of the floor is drawn");
}

#[test]
fn draws_models_where_the_entities_are() {
    let Some((device, queue)) = device_or_skip() else { return };
    // a model: a square of the missing texture, 64 units a side, facing up; it is lit from
    // where it is drawn, so its purple stays purple over the red floor
    let vertices = [[0.0, 0.0], [0.0, 64.0], [64.0, 64.0], [64.0, 0.0]]
        .map(|[x, y]| gmod_world::Vertex {
            position: [x, y, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [x / 64.0, y / 64.0],
            light: [1.0; 3],
            ..Default::default()
        })
        .to_vec();
    let surface = gmod_world::Surface {
        material: "square".to_string(),
        first: 0,
        count: 6,
        base: None,
        base2: None,
        alpha_test: None,
        translucent: false,
        additive: false,
        lit: true,
        no_cull: false,
        tint: [1.0; 3],
        alpha: 1.0,
        water: None,
        missing: true,
    };
    let square = World {
        pieces: vec![gmod_world::Piece { vertices, indices: vec![0, 1, 2, 0, 2, 3], lightmap: None, surfaces: vec![surface] }],
        textures: Vec::new(),
        sky: Default::default(),
        missing_materials: Vec::new(),
        missing_textures: Vec::new(),
        missing_models: Vec::new(),
    };
    let purple = |p: &[u8]| p[0] > 120 && p[2] > 120 && p[1] < 40;
    let above = Camera::looking_at([64.0, 64.0, 200.0], [64.0, 64.1, 0.0], 75.0);

    let mut renderer = Renderer::new(&device, &queue, FORMAT, &test_map());
    let model = renderer.add_model(&device, &queue, &square);
    // turned a quarter, its x runs along the world's y and its y along the world's -x: from
    // (96, 32) it covers the middle of the floor, above it
    renderer.set_entities(vec![Placed {
        model,
        origin: [96.0, 32.0, 32.0],
        angles: [0.0, 90.0, 0.0],
        scale: 1.0,
        light: Default::default(),
    }]);
    let pixels = picture(&device, &queue, &mut renderer, &above);
    let middle = (24..40).flat_map(|y| (24..40).map(move |x| (x, y)));
    let purples = middle.clone().filter(|&(x, y)| purple(at(&pixels, x, y))).count();
    assert!(purples > 16 * 16 / 4, "{purples} purple pixels in the middle");
    assert!(middle.clone().all(|(x, y)| !red(at(&pixels, x, y))), "the floor shows through the model");
    // the floor around it, the clear colour in the corner
    assert!((0..64 * 64).any(|i| red(at(&pixels, i % 64, i / 64))));
    let corner = at(&pixels, 0, 0);
    assert!(corner[2] > corner[0] && corner[1] > corner[0], "{corner:?}");

    // lit by the map: blue light from above (the cube's +z side) makes it blue; a blue lamp
    // above does too; one below leaves it black
    let lit = |light: gmod_world::light::ModelLight| {
        vec![Placed { model, origin: [96.0, 32.0, 32.0], angles: [0.0, 90.0, 0.0], scale: 1.0, light }]
    };
    let blue_pixels = |pixels: &[u8]| {
        (24..40)
            .flat_map(|y| (24..40).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = at(pixels, x, y);
                p[2] > 120 && p[0] < 40 && p[1] < 40
            })
            .count()
    };
    let mut cube = [[0.0; 3]; 6];
    cube[4] = [0.0, 0.0, 1.0];
    renderer.set_entities(lit(gmod_world::light::ModelLight { ambient: cube, lights: Vec::new() }));
    assert!(blue_pixels(&picture(&device, &queue, &mut renderer, &above)) > 16 * 16 / 4, "the cube's light");
    let lamp = |towards: [f32; 3]| gmod_world::light::ModelLight { ambient: [[0.0; 3]; 6], lights: vec![(towards, [0.0, 0.0, 1.0])] };
    renderer.set_entities(lit(lamp([0.0, 0.0, 100.0])));
    assert!(blue_pixels(&picture(&device, &queue, &mut renderer, &above)) > 16 * 16 / 4, "a lamp above");
    renderer.set_entities(lit(lamp([0.0, 0.0, -100.0])));
    let pixels = picture(&device, &queue, &mut renderer, &above);
    assert_eq!(blue_pixels(&pixels), 0, "a lamp below");
    assert!(!red(at(&pixels, 32, 32)), "the black model still hides the floor");
    // the lights of the moment: a light near it lights it; one for the world only does not;
    // a flashlight from above does
    renderer.set_entities(lit(gmod_world::light::ModelLight { ambient: [[0.0; 3]; 6], lights: Vec::new() }));
    let point = |models| PointLight { position: [64.0, 64.0, 64.0], color: [0.0, 0.0, 4.0], radius: 200.0, world: true, models };
    renderer.set_lights(vec![point(true)], None);
    assert!(blue_pixels(&picture(&device, &queue, &mut renderer, &above)) > 16 * 16 / 4, "a light of the moment");
    renderer.set_lights(vec![point(false)], None);
    assert_eq!(blue_pixels(&picture(&device, &queue, &mut renderer, &above)), 0, "a light not for models");
    let flashlight = SpotLight {
        position: [64.0, 64.0, 150.0],
        direction: [0.0, 0.0, -1.0],
        color: [0.0, 0.0, 4.0],
        far: 750.0,
        outer: 0.9,
        inner: 0.95,
    };
    renderer.set_lights(Vec::new(), Some(flashlight));
    assert!(blue_pixels(&picture(&device, &queue, &mut renderer, &above)) > 16 * 16 / 4, "the flashlight");
    renderer.set_lights(Vec::new(), None);
    renderer.set_entities(lit(Default::default()));

    // from below it is turned away, like the floor; with no entities it is gone
    let below = Camera::looking_at([64.0, 64.0, 10.0], [64.0, 64.1, 100.0], 75.0);
    let pixels = picture(&device, &queue, &mut renderer, &below);
    assert!((0..64 * 64).all(|i| !purple(at(&pixels, i % 64, i / 64))), "the back of the model is drawn");
    renderer.set_entities(Vec::new());
    let pixels = picture(&device, &queue, &mut renderer, &above);
    assert!((0..64 * 64).all(|i| !purple(at(&pixels, i % 64, i / 64))), "a model is drawn with no entity");
}

#[test]
fn draws_the_2d_over_the_picture() {
    use gmod_ui::{Canvas, Clip, Texture};
    let Some((device, queue)) = device_or_skip() else { return };
    let mut overlay = Overlay::new(&device, &queue, FORMAT);
    let mut canvas = Canvas::new(64, 64);
    // a red square at the top left; a green one at the top right, cut by the GPU to a quarter
    canvas.rect(0.0, 0.0, 32.0, 32.0, [255, 0, 0, 255]);
    canvas.clip = Some(Clip::new(32, 0, 16, 16));
    let square = [[32.0, 0.0, 0.0, 0.0], [64.0, 0.0, 0.0, 0.0], [64.0, 32.0, 0.0, 0.0], [32.0, 32.0, 0.0, 0.0]];
    canvas.polygon(Texture::White, false, &square, [0, 255, 0, 255]);
    canvas.clip = None;
    // an image at the bottom left; red light added at the bottom right
    canvas.image(Texture::Image(7), false, [0.0, 32.0, 32.0, 32.0], [0.0, 0.0, 1.0, 1.0], [255; 4]);
    canvas.image(Texture::White, true, [32.0, 32.0, 32.0, 32.0], [0.0, 0.0, 1.0, 1.0], [255, 0, 0, 128]);
    assert_eq!(overlay.missing_images(&canvas), [7]);
    overlay.set_image(&device, &queue, 7, Some((1, 1, &[0, 0, 255, 255])));
    assert!(overlay.missing_images(&canvas).is_empty());

    let pixels = read_back(&device, &queue, |encoder, view| {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
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
        drop(pass);
        overlay.render(&device, &queue, encoder, view, (64, 64), &canvas);
    });
    assert!(red(at(&pixels, 8, 8)), "{:?}", at(&pixels, 8, 8));
    let green = |p: &[u8]| p[1] > 200 && p[0] < 40 && p[2] < 40;
    assert!(green(at(&pixels, 40, 8)), "{:?}", at(&pixels, 40, 8));
    assert_eq!(&at(&pixels, 56, 8)[..3], [0, 0, 0], "the clip was not kept");
    assert_eq!(&at(&pixels, 40, 24)[..3], [0, 0, 0], "the clip was not kept");
    let blue = at(&pixels, 8, 40);
    assert!(blue[2] > 200 && blue[0] < 40 && blue[1] < 40, "{blue:?}");
    let added = at(&pixels, 40, 40);
    assert!(added[0] > 100 && added[0] < 255 && added[1] < 40, "{added:?}");
}
