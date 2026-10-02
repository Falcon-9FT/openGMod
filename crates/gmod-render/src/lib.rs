//! A map on the GPU with wgpu: Direct3D 12 on Windows, Vulkan as the fallback.
//!
//! [`Renderer::new`] uploads a [`World`] as [`gmod_world::gpu`] lays it out: one vertex and
//! one index buffer, the textures (DXT stays compressed as BC1-BC3 where the GPU takes it),
//! a light map per brush model. Models of entities are uploaded the same way when they are
//! first needed ([`Renderer::add_model`]) and drawn where the entities are
//! ([`Renderer::set_entities`]), lit from above by their turned normals. [`Renderer::render`]
//! draws a frame: the sky box, the opaque surfaces of the world and the entities, then the
//! translucent ones - the world's from the farthest, then the entities'. Depth runs from 1 at
//! the eye to 0 at infinity.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::num::NonZeroU64;

use gmod_world::gpu::{self, Camera, Draw, PixelFormat, TextureData};
use gmod_world::{Texture, World};

/// Runs a future to its end on this thread: what setting up a GPU needs, without a runtime.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(std::sync::Arc::new(Unpark(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(output) => return output,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

/// The depth buffer's format.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The largest texture uploaded; bigger ones start at a smaller mipmap.
const MAX_TEXTURE: u32 = 4096;
/// Bytes between the parameters of two surfaces in their buffer: the usual offset alignment.
const PARAMS_STRIDE: u64 = 256;
/// Bytes of a surface's parameters.
const PARAMS_SIZE: u64 = 48;
/// Bytes of a camera for the shaders: its matrix, its eye, where the light of models comes from.
const CAMERA_SIZE: u64 = 96;
/// Bytes between two cameras in their buffer: the world's, then one for each entity drawn.
const CAMERA_STRIDE: u64 = 256;

/// Which pipeline a surface needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Opaque,
    OpaqueTwoSided,
    Blend,
    BlendTwoSided,
    Additive,
}

impl Kind {
    fn translucent(self) -> bool {
        !matches!(self, Kind::Opaque | Kind::OpaqueTwoSided)
    }
}

struct Pipelines {
    opaque: wgpu::RenderPipeline,
    opaque_two_sided: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
    blend_two_sided: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    sky: wgpu::RenderPipeline,
}

impl Pipelines {
    fn get(&self, kind: Kind) -> &wgpu::RenderPipeline {
        match kind {
            Kind::Opaque => &self.opaque,
            Kind::OpaqueTwoSided => &self.opaque_two_sided,
            Kind::Blend => &self.blend,
            Kind::BlendTwoSided => &self.blend_two_sided,
            Kind::Additive => &self.additive,
        }
    }
}

/// A model added to a [`Renderer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelId(usize);

/// A model where an entity is: its origin, its angles (pitch, yaw, roll in degrees) and how
/// much bigger than it was made it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub model: ModelId,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub scale: f32,
}

/// An uploaded texture and which sampler it takes; none when it could not be uploaded.
type Uploaded = Option<(wgpu::TextureView, usize)>;

/// Pieces on the GPU - the world's or a model's - and what each of their surfaces is drawn
/// with.
struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    draws: Vec<(Draw, Kind)>,
    surface_groups: Vec<wgpu::BindGroup>,
    lightmap_groups: Vec<wgpu::BindGroup>,
}

/// What can be drawn of a world, on the GPU.
pub struct Renderer {
    shared: Shared,
    pipelines: Pipelines,
    /// The cameras: the world's, then one for each entity drawn, `CAMERA_STRIDE` apart.
    camera_buffer: wgpu::Buffer,
    camera_group: wgpu::BindGroup,
    camera_slots: usize,
    sky_camera_buffer: wgpu::Buffer,
    sky_camera_group: wgpu::BindGroup,
    world: Mesh,
    models: Vec<Mesh>,
    /// Textures by name, the world's and the models', each uploaded once.
    textures: HashMap<String, Uploaded>,
    entities: Vec<Placed>,
    sky_vertices: Option<wgpu::Buffer>,
    sky_groups: Vec<wgpu::BindGroup>,
    depth: Option<(wgpu::TextureView, (u32, u32))>,
    /// Textures that could not be uploaded and are drawn as missing.
    pub failed_textures: Vec<String>,
}

fn buffer(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, usage: wgpu::BufferUsages, data: &[u8]) -> wgpu::Buffer {
    // writes go in whole words; an empty buffer still needs a size
    let size = (data.len() as u64).div_ceil(4).max(1) * 4;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size,
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut padded = data.to_vec();
    padded.resize(size as usize, 0);
    queue.write_buffer(&buffer, 0, &padded);
    buffer
}

fn wgpu_format(format: PixelFormat) -> wgpu::TextureFormat {
    match format {
        PixelFormat::Bc1 => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
        PixelFormat::Bc2 => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
        PixelFormat::Bc3 => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
        PixelFormat::Bgra8 => wgpu::TextureFormat::Bgra8UnormSrgb,
        PixelFormat::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
    }
}

fn upload_texture(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, data: &TextureData) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: data.width, height: data.height, depth_or_array_layers: 1 },
        mip_level_count: data.levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu_format(data.format),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (side, _) = data.format.block();
    for (level, bytes) in data.levels.iter().enumerate() {
        let (width, height) = ((data.width >> level).max(1), (data.height >> level).max(1));
        let (row, rows) = data.format.layout(width, height);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(rows) },
            // compressed levels are copied in whole blocks
            wgpu::Extent3d { width: width.div_ceil(side) * side, height: height.div_ceil(side) * side, depth_or_array_layers: 1 },
        );
    }
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn upload_lightmap(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32, bytes: &[u8]) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("light map"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        bytes,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 8), rows_per_image: Some(height) },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn sampler(device: &wgpu::Device, clamp_s: bool, clamp_t: bool, filtered: bool) -> wgpu::Sampler {
    let mode = |clamp: bool| if clamp { wgpu::AddressMode::ClampToEdge } else { wgpu::AddressMode::Repeat };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("sampler"),
        address_mode_u: mode(clamp_s),
        address_mode_v: mode(clamp_t),
        address_mode_w: wgpu::AddressMode::Repeat,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Linear,
        lod_min_clamp: 0.0,
        lod_max_clamp: 32.0,
        compare: None,
        anisotropy_clamp: if filtered { 16 } else { 1 },
        border_color: None,
    })
}

/// An uploaded texture and which sampler it takes, or the missing texture.
fn pick<'a>(
    textures: &'a [Option<(wgpu::TextureView, usize)>],
    missing: &'a wgpu::TextureView,
    index: Option<usize>,
) -> (&'a wgpu::TextureView, usize) {
    match index.and_then(|i| textures.get(i)).and_then(Option::as_ref) {
        Some((view, sampler)) => (view, *sampler),
        None => (missing, 0),
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

/// A camera for the shaders, padded to `CAMERA_STRIDE`: its matrix, its eye, and for a model
/// where the light comes from in the model's space (w 1; 0 for the world, lit as it was baked).
fn camera_bytes(matrix: [f32; 16], eye: [f32; 3], sun: Option<[f32; 3]>) -> Vec<u8> {
    let sun = sun.map_or([0.0; 4], |[x, y, z]| [x, y, z, 1.0]);
    let mut bytes: Vec<u8> = matrix.iter().chain(&eye).chain(&[1.0]).chain(&sun).flat_map(|v| v.to_le_bytes()).collect();
    bytes.resize(CAMERA_STRIDE as usize, 0);
    bytes
}

/// A bind group of one camera in a buffer of them, chosen by its offset when drawing.
fn camera_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, buffer: &wgpu::Buffer) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("camera"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer, offset: 0, size: NonZeroU64::new(CAMERA_SIZE) }),
        }],
    })
}

fn camera_buffer(device: &wgpu::Device, slots: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cameras"),
        size: slots as u64 * CAMERA_STRIDE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

struct Layouts {
    camera: wgpu::BindGroupLayout,
    surface: wgpu::BindGroupLayout,
    texture: wgpu::BindGroupLayout,
}

/// What uploading needs after the world is up: the layouts, the samplers, the missing texture
/// and the white light map of what is lit per vertex.
struct Shared {
    layouts: Layouts,
    samplers: Vec<wgpu::Sampler>,
    missing: wgpu::TextureView,
    white: wgpu::TextureView,
    lightmap_sampler: wgpu::Sampler,
    compressed: bool,
}

impl Shared {
    fn texture(&self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &Texture) -> Uploaded {
        let data = gpu::texture_data(&texture.vtf, self.compressed, MAX_TEXTURE)?;
        let view = upload_texture(device, queue, &texture.name, &data);
        Some((view, usize::from(data.clamp_s) | usize::from(data.clamp_t) << 1))
    }

    /// Uploads the pieces of `world`, whose textures are `textures`.
    fn mesh(&self, device: &wgpu::Device, queue: &wgpu::Queue, world: &World, textures: &[Uploaded]) -> Mesh {
        let texture = |index: Option<usize>| pick(textures, &self.missing, index);
        let geometry = gpu::geometry(world);
        let vertices = buffer(device, queue, "vertices", wgpu::BufferUsages::VERTEX, &geometry.vertices);
        let indices = buffer(device, queue, "indices", wgpu::BufferUsages::INDEX, &geometry.indices);
        let mut params = vec![0u8; (geometry.draws.len() as u64 * PARAMS_STRIDE).max(PARAMS_STRIDE) as usize];
        let mut draws = Vec::with_capacity(geometry.draws.len());
        for (i, draw) in geometry.draws.iter().enumerate() {
            let piece = &world.pieces[draw.piece];
            let surface = &piece.surfaces[draw.surface];
            let values = gpu::surface_params(surface, piece.lightmap.is_some());
            let at = i * PARAMS_STRIDE as usize;
            for (k, v) in values.iter().enumerate() {
                params[at + k * 4..at + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            let kind = match (surface.translucent, surface.additive, surface.no_cull) {
                (_, true, _) => Kind::Additive,
                (true, _, false) => Kind::Blend,
                (true, _, true) => Kind::BlendTwoSided,
                (false, _, false) => Kind::Opaque,
                (false, _, true) => Kind::OpaqueTwoSided,
            };
            draws.push((*draw, kind));
        }
        let params_buffer = buffer(device, queue, "surfaces", wgpu::BufferUsages::UNIFORM, &params);
        let surface_groups = geometry
            .draws
            .iter()
            .enumerate()
            .map(|(i, draw)| {
                let surface = &world.pieces[draw.piece].surfaces[draw.surface];
                let (base, sampler_index) = texture(surface.base);
                let (blend, _) = texture(surface.base2.or(surface.base));
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("surface"),
                    layout: &self.layouts.surface,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &params_buffer,
                                offset: i as u64 * PARAMS_STRIDE,
                                size: NonZeroU64::new(PARAMS_SIZE),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(base) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(blend) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.samplers[sampler_index]) },
                    ],
                })
            })
            .collect();
        // light maps; what is lit per vertex gets the white one
        let lightmap_groups = world
            .pieces
            .iter()
            .map(|piece| {
                let view = piece
                    .lightmap
                    .as_ref()
                    .filter(|l| l.width > 0 && l.height > 0)
                    .map(|l| upload_lightmap(device, queue, l.width, l.height, &gpu::lightmap_data(l)));
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("light map"),
                    layout: &self.layouts.texture,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(view.as_ref().unwrap_or(&self.white)),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.lightmap_sampler) },
                    ],
                })
            })
            .collect();
        Mesh { vertices, indices, draws, surface_groups, lightmap_groups }
    }
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    kind: Kind,
) -> wgpu::RenderPipeline {
    let attributes = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 1 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 32, shader_location: 2 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 40, shader_location: 3 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 44, shader_location: 4 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 5 },
    ];
    let buffers = [Some(wgpu::VertexBufferLayout {
        array_stride: gpu::VERTEX_SIZE as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    })];
    let additive = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        },
    };
    let blend = match kind {
        Kind::Opaque | Kind::OpaqueTwoSided => None,
        Kind::Blend | Kind::BlendTwoSided => Some(wgpu::BlendState::ALPHA_BLENDING),
        Kind::Additive => Some(additive),
    };
    let two_sided = matches!(kind, Kind::OpaqueTwoSided | Kind::BlendTwoSided | Kind::Additive);
    let targets = [Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("world"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // the game's front faces turn clockwise
            front_face: wgpu::FrontFace::Cw,
            cull_mode: if two_sided { None } else { Some(wgpu::Face::Back) },
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(!kind.translucent()),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn sky_pipeline(device: &wgpu::Device, layouts: &Layouts, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sky"),
        source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sky"),
        bind_group_layouts: &[Some(&layouts.camera), Some(&layouts.texture)],
        immediate_size: 0,
    });
    let attributes = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 12, shader_location: 1 },
    ];
    let buffers = [Some(wgpu::VertexBufferLayout { array_stride: 20, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })];
    let targets = [Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sky"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &buffers,
        },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl Renderer {
    /// Uploads a world. `format` is the format of the views it will draw into (sRGB).
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, world: &World) -> Renderer {
        let camera_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(CAMERA_SIZE),
            },
            count: None,
        };
        let layouts = Layouts {
            camera: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("camera"), entries: &[camera_entry] }),
            surface: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("surface"),
                entries: &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT), texture_entry(1), texture_entry(2), sampler_entry(3)],
            }),
            texture: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture"),
                entries: &[texture_entry(0), sampler_entry(1)],
            }),
        };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world"),
            source: wgpu::ShaderSource::Wgsl(include_str!("world.wgsl").into()),
        });
        let world_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world"),
            bind_group_layouts: &[Some(&layouts.camera), Some(&layouts.surface), Some(&layouts.texture)],
            immediate_size: 0,
        });
        let pipelines = Pipelines {
            opaque: pipeline(device, &world_layout, &shader, format, Kind::Opaque),
            opaque_two_sided: pipeline(device, &world_layout, &shader, format, Kind::OpaqueTwoSided),
            blend: pipeline(device, &world_layout, &shader, format, Kind::Blend),
            blend_two_sided: pipeline(device, &world_layout, &shader, format, Kind::BlendTwoSided),
            additive: pipeline(device, &world_layout, &shader, format, Kind::Additive),
            sky: sky_pipeline(device, &layouts, format),
        };

        let camera_slots = 1;
        let sky_camera_buffer = camera_buffer(device, 1);
        let sky_camera_group = camera_group(device, &layouts.camera, &sky_camera_buffer);
        let camera_buffer = camera_buffer(device, camera_slots);
        let camera_group = camera_group(device, &layouts.camera, &camera_buffer);

        let shared = Shared {
            samplers: (0..4).map(|i| sampler(device, i & 1 != 0, i & 2 != 0, true)).collect(),
            missing: upload_texture(device, queue, "missing", &gpu::missing_texture()),
            white: upload_lightmap(
                device,
                queue,
                1,
                1,
                &gpu::lightmap_data(&gmod_world::Lightmap { width: 1, height: 1, texels: vec![[1.0; 3]] }),
            ),
            lightmap_sampler: sampler(device, true, true, false),
            compressed: device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            layouts,
        };

        // textures: what cannot be uploaded is drawn as missing
        let mut failed_textures = Vec::new();
        let mut by_name = HashMap::new();
        let textures: Vec<Uploaded> = world
            .textures
            .iter()
            .map(|texture| {
                let uploaded = shared.texture(device, queue, texture);
                if uploaded.is_none() {
                    failed_textures.push(texture.name.clone());
                }
                by_name.insert(texture.name.clone(), uploaded.clone());
                uploaded
            })
            .collect();
        let mesh = shared.mesh(device, queue, world, &textures);

        // the sky box: two triangles a face
        let quads = gpu::sky_quads(world);
        let sky_sampler = sampler(device, true, true, false);
        let mut sky_bytes = Vec::new();
        let mut sky_groups = Vec::new();
        for quad in &quads {
            for corner in [0, 1, 2, 0, 2, 3] {
                sky_bytes.extend(quad.corners[corner].iter().flat_map(|v| v.to_le_bytes()));
            }
            let (view, _) = pick(&textures, &shared.missing, Some(quad.texture));
            sky_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky"),
                layout: &shared.layouts.texture,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sky_sampler) },
                ],
            }));
        }
        let sky_vertices = (!quads.is_empty()).then(|| buffer(device, queue, "sky", wgpu::BufferUsages::VERTEX, &sky_bytes));

        Renderer {
            shared,
            pipelines,
            camera_buffer,
            camera_group,
            camera_slots,
            sky_camera_buffer,
            sky_camera_group,
            world: mesh,
            models: Vec::new(),
            textures: by_name,
            entities: Vec::new(),
            sky_vertices,
            sky_groups,
            depth: None,
            failed_textures,
        }
    }

    /// Uploads a model ([`gmod_world::model`]) to draw where entities have it. Textures the
    /// world or another model already has are not uploaded again.
    pub fn add_model(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, model: &World) -> ModelId {
        let textures: Vec<Uploaded> = model
            .textures
            .iter()
            .map(|texture| {
                if let Some(known) = self.textures.get(&texture.name) {
                    return known.clone();
                }
                let uploaded = self.shared.texture(device, queue, texture);
                if uploaded.is_none() {
                    self.failed_textures.push(texture.name.clone());
                }
                self.textures.insert(texture.name.clone(), uploaded.clone());
                uploaded
            })
            .collect();
        self.models.push(self.shared.mesh(device, queue, model, &textures));
        ModelId(self.models.len() - 1)
    }

    /// The models to draw from now on, and where.
    pub fn set_entities(&mut self, entities: Vec<Placed>) {
        self.entities = entities.into_iter().filter(|e| e.model.0 < self.models.len()).collect();
    }

    fn depth_view(&mut self, device: &wgpu::Device, size: (u32, u32)) -> wgpu::TextureView {
        if let Some((view, made)) = &self.depth
            && *made == size
        {
            return view.clone();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth = Some((view.clone(), size));
        view
    }

    /// Draws a frame into `target`, a view of `size` pixels.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        camera: &Camera,
    ) {
        let depth = self.depth_view(device, size);
        let aspect = size.0.max(1) as f32 / size.1.max(1) as f32;
        let view_projection = camera.view_projection(aspect);
        // the world's camera, then each entity's: the camera after the entity's placement
        let slots = 1 + self.entities.len();
        if slots > self.camera_slots {
            self.camera_slots = slots.next_power_of_two();
            self.camera_buffer = camera_buffer(device, self.camera_slots);
            self.camera_group = camera_group(device, &self.shared.layouts.camera, &self.camera_buffer);
        }
        let mut cameras = camera_bytes(view_projection, camera.eye, None);
        for placed in &self.entities {
            let (mut matrix, sun) = gpu::placement(placed.origin, placed.angles);
            matrix[..12].iter_mut().for_each(|v| *v *= placed.scale);
            cameras.extend(camera_bytes(gpu::multiply(&view_projection, &matrix), camera.eye, Some(sun)));
        }
        queue.write_buffer(&self.camera_buffer, 0, &cameras);
        queue.write_buffer(&self.sky_camera_buffer, 0, &camera_bytes(camera.sky_projection(aspect), [0.0; 3], None));
        // translucent surfaces from the farthest
        let distance = |draw: &Draw| {
            let d = [draw.center[0] - camera.eye[0], draw.center[1] - camera.eye[1], draw.center[2] - camera.eye[2]];
            d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
        };
        let draws = &self.world.draws;
        let mut opaque: Vec<usize> = (0..draws.len()).filter(|&i| !draws[i].1.translucent()).collect();
        // opaque ones by pipeline, to switch less
        opaque.sort_by_key(|&i| draws[i].1 as u8);
        let mut translucent: Vec<usize> = (0..draws.len()).filter(|&i| draws[i].1.translucent()).collect();
        translucent.sort_by(|&a, &b| distance(&draws[b].0).total_cmp(&distance(&draws[a].0)));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("world"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.25, g: 0.35, b: 0.5, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(sky) = &self.sky_vertices {
            pass.set_pipeline(&self.pipelines.sky);
            pass.set_bind_group(0, &self.sky_camera_group, &[0]);
            pass.set_vertex_buffer(0, sky.slice(..));
            for (i, group) in self.sky_groups.iter().enumerate() {
                pass.set_bind_group(1, group, &[]);
                pass.draw(i as u32 * 6..i as u32 * 6 + 6, 0..1);
            }
        }
        let mut current = None;
        draw_mesh(&mut pass, &self.pipelines, &mut current, &self.camera_group, 0, &self.world, opaque);
        let entity = |slot: usize| ((slot + 1) as u64 * CAMERA_STRIDE) as u32;
        for (slot, placed) in self.entities.iter().enumerate() {
            let mesh = &self.models[placed.model.0];
            let list = (0..mesh.draws.len()).filter(|&i| !mesh.draws[i].1.translucent());
            draw_mesh(&mut pass, &self.pipelines, &mut current, &self.camera_group, entity(slot), mesh, list);
        }
        draw_mesh(&mut pass, &self.pipelines, &mut current, &self.camera_group, 0, &self.world, translucent);
        for (slot, placed) in self.entities.iter().enumerate() {
            let mesh = &self.models[placed.model.0];
            let list = (0..mesh.draws.len()).filter(|&i| mesh.draws[i].1.translucent());
            draw_mesh(&mut pass, &self.pipelines, &mut current, &self.camera_group, entity(slot), mesh, list);
        }
    }
}

/// Draws the surfaces `list` of a mesh, seen through the camera at `offset`.
fn draw_mesh(
    pass: &mut wgpu::RenderPass,
    pipelines: &Pipelines,
    current: &mut Option<Kind>,
    camera: &wgpu::BindGroup,
    offset: u32,
    mesh: &Mesh,
    list: impl IntoIterator<Item = usize>,
) {
    pass.set_bind_group(0, camera, &[offset]);
    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
    for i in list {
        let (draw, kind) = mesh.draws[i];
        if draw.index_count == 0 {
            continue;
        }
        if *current != Some(kind) {
            pass.set_pipeline(pipelines.get(kind));
            *current = Some(kind);
        }
        pass.set_bind_group(1, &mesh.surface_groups[i], &[]);
        pass.set_bind_group(2, &mesh.lightmap_groups[draw.piece], &[]);
        pass.draw_indexed(draw.first_index..draw.first_index + draw.index_count, 0, 0..1);
    }
}

#[cfg(test)]
mod tests;
