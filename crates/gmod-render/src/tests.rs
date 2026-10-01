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

/// Draws the test map from `camera` into a 64 x 64 picture and reads it back, RGBA.
fn draw(device: &wgpu::Device, queue: &wgpu::Queue, world: &World, camera: &Camera) -> Vec<u8> {
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = Renderer::new(device, queue, format, world);
    let (width, height) = (64u32, 64u32);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("picture"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("test") });
    renderer.render(device, queue, &mut encoder, &view, (width, height), camera);
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
    let Some((device, queue)) = device() else {
        println!("no GPU here, not even a software one: skipped");
        return;
    };
    // without the game, every material is missing: purple and black squares, lit by the light
    // maps - the floor's is red
    let bsp = gmod_bsp::Bsp::parse("test.bsp".into(), Arc::from(gmod_bsp::testmap::bytes())).unwrap();
    let world = gmod_world::load(&gmod_fs::Vfs::new(), &bsp);
    let red = |p: &[u8]| p[0] > 200 && p[1] < 40 && p[2] < 40;
    let at = |pixels: &[u8], x: usize, y: usize| pixels[y * 256 + x * 4..y * 256 + x * 4 + 4].to_vec();

    // from above, looking down at the floor: red (and black) in the middle, the clear colour
    // around it
    let above = draw(&device, &queue, &world, &Camera::looking_at([64.0, 64.0, 200.0], [64.0, 64.1, 0.0], 75.0));
    let reds = (0..64 * 64).filter(|i| red(&at(&above, i % 64, i / 64))).count();
    assert!(reds > 64 * 64 / 10, "{reds} red pixels");
    let corner = at(&above, 0, 0);
    assert!(corner[2] > corner[0] && corner[1] > corner[0], "{corner:?}");

    // from below the floor is turned away and not drawn: nothing red
    let below = draw(&device, &queue, &world, &Camera::looking_at([64.0, 64.0, -200.0], [64.0, 64.1, 0.0], 75.0));
    let reds = (0..64 * 64).filter(|i| red(&at(&below, i % 64, i / 64))).count();
    assert_eq!(reds, 0, "the back of the floor is drawn");
}
