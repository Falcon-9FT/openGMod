//! A map on the GPU with wgpu: Direct3D 12 on Windows, Vulkan as the fallback.
//!
//! [`Renderer::new`] uploads a [`World`] as [`gmod_world::gpu`] lays it out: one vertex and
//! one index buffer, the textures (DXT stays compressed as BC1-BC3 where the GPU takes it),
//! a light map per brush model. Models of entities are uploaded the same way when they are
//! first needed ([`Renderer::add_model`]) and drawn where the entities are
//! ([`Renderer::set_entities`]), lit by the light of the map where they stand
//! ([`gmod_world::light`]): the ambient cube and the lights that see them. The lights of the
//! moment - the game's `DynamicLight`s and the flashlight ([`Renderer::set_lights`]) - light
//! the world and the models over their baked light. A model that moves is posed by the
//! matrices of its bones ([`Placed::bones`]): the vertex shader moves each vertex by the bones
//! it is weighted to. [`Renderer::render`] draws a frame: the sky box, then the 3D sky box
//! ([`gmod_world::sky`]) seen from its camera, then - depth cleared, so that all of it is
//! behind - the opaque surfaces of the world and the entities, then the translucent ones - the
//! world's from the farthest, then the entities'; then the view models (the weapon in the
//! player's hands) with their own field of view, over everything
//! ([`Renderer::set_view_models`]). Fog fades what is far towards its colour: the world's fog
//! in the world, the sky camera's in the 3D sky box. Depth runs from 1 at the eye to 0 at
//! infinity. Over it an [`Overlay`] draws the 2D of the HUD and the panels.
//!
//! Materials change as time goes on ([`Renderer::set_time`]): every frame the surfaces whose
//! materials have proxies get their parameters anew ([`gmod_world::Surface::look`]) - the
//! layer of an animated texture, whose frames are uploaded as the layers of one texture, the
//! transform of its coordinates, its tint and alpha. Overlays come with the world's surfaces,
//! lifted off the faces they lie on; `DecalModulate` ones darken and lighten what is behind.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::num::NonZeroU64;

use gmod_world::gpu::{self, Camera, Draw, PixelFormat, TextureData};
use gmod_world::sky::SkyCamera;
use gmod_world::{Piece, Surface, Texture, World};

mod ui;

pub use ui::Overlay;

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
const PARAMS_SIZE: u64 = gpu::SURFACE_PARAMS as u64 * 4;
/// Bytes of a camera for the shaders: its matrix, its eye, where the light of models comes
/// from, the lights of the moment, where the bones of a posed model are, and the fog.
const CAMERA_SIZE: u64 = 464;
/// Bytes of a bone's matrix for the shaders: three rows of four.
const BONE_SIZE: u64 = 48;
/// Nothing of a view model closer than this is drawn: a weapon is held close to the eyes.
const VIEW_MODEL_NEAR: f32 = 1.0;
/// Bytes between two cameras in their buffer: the world's, then one for each entity drawn.
const CAMERA_STRIDE: u64 = 512;

/// Which pipeline a surface needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Opaque,
    OpaqueTwoSided,
    Blend,
    BlendTwoSided,
    Additive,
    /// Twice its colour times what is behind it.
    Modulate,
}

impl Kind {
    fn translucent(self) -> bool {
        !matches!(self, Kind::Opaque | Kind::OpaqueTwoSided)
    }
}

const KINDS: [Kind; 6] = [Kind::Opaque, Kind::OpaqueTwoSided, Kind::Blend, Kind::BlendTwoSided, Kind::Additive, Kind::Modulate];

/// The pipelines of each kind of surface: for what stands still, and for models posed by their
/// bones.
struct Pipelines {
    still: Vec<wgpu::RenderPipeline>,
    posed: Vec<wgpu::RenderPipeline>,
    sky: wgpu::RenderPipeline,
}

impl Pipelines {
    fn get(&self, kind: Kind, posed: bool) -> &wgpu::RenderPipeline {
        &(if posed { &self.posed } else { &self.still })[kind as usize]
    }
}

/// A model added to a [`Renderer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ModelId(usize);

/// A model where an entity is: its origin, its angles (pitch, yaw, roll in degrees) and how
/// much bigger than it was made it is drawn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Placed {
    pub model: ModelId,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub scale: f32,
    /// The light of the map where it stands ([`gmod_world::light::MapLight::at`]).
    pub light: gmod_world::light::ModelLight,
    /// What moves each bone's vertices from where the model was made with them to where it is
    /// posed, in the model's space (`gmod_model::Rig::skinning`); empty: as it was made. Fewer
    /// than its vertices name are not used.
    pub bones: Vec<[[f32; 4]; 3]>,
}

/// The most lights of the moment a surface takes, besides the flashlight.
pub const MAX_POINT_LIGHTS: usize = 4;

/// A light of the moment (`DynamicLight`): it falls off to nothing at its radius.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointLight {
    pub position: [f32; 3],
    /// Linear light at its middle.
    pub color: [f32; 3],
    pub radius: f32,
    /// It lights the world, and the models of entities.
    pub world: bool,
    pub models: bool,
}

/// A cone of light: the flashlight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpotLight {
    pub position: [f32; 3],
    /// Where it points, of length 1.
    pub direction: [f32; 3],
    pub color: [f32; 3],
    /// How far it reaches, falling off to nothing there.
    pub far: f32,
    /// The cosines of the cone's edge, where it is gone, and of where it starts to fade.
    pub outer: f32,
    pub inner: f32,
}

/// A point of the world in a model's space, by the model's placement (origin, axes, scale).
fn model_space(p: [f32; 3], origin: [f32; 3], axes: [[f32; 3]; 3], scale: f32) -> [f32; 3] {
    let d = [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]];
    let s = 1.0 / scale.max(1e-6);
    axes.map(|a| (a[0] * d[0] + a[1] * d[1] + a[2] * d[2]) * s)
}

/// The lights of the moment in a space of their own: a model's, by its placement (origin,
/// axes, scale); the world's is the identity.
fn lights_bytes(
    points: &[PointLight],
    spot: Option<&SpotLight>,
    models: bool,
    origin: [f32; 3],
    axes: [[f32; 3]; 3],
    scale: f32,
) -> Vec<f32> {
    let into = |p: [f32; 3]| model_space(p, origin, axes, scale);
    let turn = |d: [f32; 3]| axes.map(|a| a[0] * d[0] + a[1] * d[1] + a[2] * d[2]);
    let lit: Vec<&PointLight> = points.iter().filter(|p| if models { p.models } else { p.world }).take(MAX_POINT_LIGHTS).collect();
    let mut values = Vec::with_capacity(4 * (2 * MAX_POINT_LIGHTS + 3));
    for i in 0..MAX_POINT_LIGHTS {
        match lit.get(i) {
            Some(p) => {
                values.extend(into(p.position));
                values.push(p.radius / scale.max(1e-6));
            }
            None => values.extend([0.0; 4]),
        }
    }
    for i in 0..MAX_POINT_LIGHTS {
        let color = lit.get(i).map_or([0.0; 3], |p| p.color);
        values.extend([color[0], color[1], color[2], 0.0]);
    }
    match spot {
        Some(s) => {
            values.extend(into(s.position));
            values.push(s.far / scale.max(1e-6));
            values.extend(turn(s.direction));
            values.push(s.outer);
            values.extend(s.color);
            values.push(s.inner);
        }
        None => values.extend([0.0; 12]),
    }
    values
}

/// A texture on the GPU: its frames as layers, its first frame alone (for the sky box), which
/// sampler it takes and how many frames it has.
#[derive(Clone)]
struct Views {
    layers: wgpu::TextureView,
    first: wgpu::TextureView,
    sampler: usize,
    frames: u32,
}

/// An uploaded texture; none when it could not be uploaded.
type Uploaded = Option<Views>;

/// A surface whose look changes with time: its draw, the surface, the frames of its base
/// texture and whether its piece has a light map.
struct Animated {
    draw: usize,
    surface: Surface,
    frames: u32,
    lightmap: bool,
}

/// Pieces on the GPU - the world's or a model's - and what each of their surfaces is drawn
/// with.
struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    draws: Vec<(Draw, Kind)>,
    surface_groups: Vec<wgpu::BindGroup>,
    lightmap_groups: Vec<wgpu::BindGroup>,
    /// For a model that moves: the bones of each vertex, and how many bones it needs.
    skin: Option<(wgpu::Buffer, usize)>,
    /// The parameters of its surfaces, `PARAMS_STRIDE` apart, and those that change with time.
    params: wgpu::Buffer,
    animated: Vec<Animated>,
}

impl Mesh {
    /// Writes the parameters of the surfaces that change with time as they are at `time`.
    fn animate(&self, queue: &wgpu::Queue, time: f32) {
        for a in &self.animated {
            let values = gpu::surface_params(&a.surface, &a.surface.look(time), a.frames, a.lightmap);
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            queue.write_buffer(&self.params, a.draw as u64 * PARAMS_STRIDE, &bytes);
        }
    }
}

/// What can be drawn of a world, on the GPU.
pub struct Renderer {
    shared: Shared,
    pipelines: Pipelines,
    /// The cameras: the world's, then one for each entity drawn, `CAMERA_STRIDE` apart.
    camera_buffer: wgpu::Buffer,
    camera_group: wgpu::BindGroup,
    camera_slots: usize,
    /// The cameras of the sky box and of the 3D sky box.
    sky_camera_buffer: wgpu::Buffer,
    sky_camera_group: wgpu::BindGroup,
    world: Mesh,
    /// The 3D sky box and its camera.
    skybox: Option<(Mesh, SkyCamera)>,
    /// The world's fog, as the shaders take it.
    fog: [f32; 8],
    models: Vec<Mesh>,
    /// Textures by name, the world's and the models', each uploaded once.
    textures: HashMap<String, Uploaded>,
    entities: Vec<Placed>,
    /// The lights of the moment: the game's `DynamicLight`s, and the flashlight.
    points: Vec<PointLight>,
    spot: Option<SpotLight>,
    /// The view models, and the field of view they are drawn with.
    view_models: Vec<Placed>,
    view_model_fov: f32,
    /// The bones of the posed models of a frame, and room for how many.
    bones_buffer: wgpu::Buffer,
    bones_group: wgpu::BindGroup,
    bones_room: usize,
    sky_vertices: Option<wgpu::Buffer>,
    sky_groups: Vec<wgpu::BindGroup>,
    depth: Option<(wgpu::TextureView, (u32, u32))>,
    /// Seconds into the map, for materials that change with time.
    time: f32,
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

/// Uploads a texture, its frames as layers: a view of them all, and one of the first.
fn upload_texture(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, data: &TextureData) -> (wgpu::TextureView, wgpu::TextureView) {
    let layers = data.frames.max(1);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: data.width, height: data.height, depth_or_array_layers: layers },
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
            wgpu::Extent3d { width: width.div_ceil(side) * side, height: height.div_ceil(side) * side, depth_or_array_layers: layers },
        );
    }
    let view = |dimension, count| {
        texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(dimension),
            array_layer_count: Some(count),
            ..Default::default()
        })
    };
    (view(wgpu::TextureViewDimension::D2Array, layers), view(wgpu::TextureViewDimension::D2, 1))
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

/// An uploaded texture, or the missing texture.
fn pick<'a>(textures: &'a [Uploaded], missing: &'a Views, index: Option<usize>) -> &'a Views {
    index.and_then(|i| textures.get(i)).and_then(Option::as_ref).unwrap_or(missing)
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    view_entry(binding, wgpu::TextureViewDimension::D2)
}

fn view_entry(binding: u32, view_dimension: wgpu::TextureViewDimension) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension,
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

/// A camera for the shaders, padded to `CAMERA_STRIDE`: its matrix; its eye in the space of
/// the vertices and how many units of the world one of theirs is (for the distance of fog);
/// for a model its light in the model's space - the ambient cube (w 1 on the first side; 0
/// for the world, lit as it was baked), then the directions towards its lights (w 1 for a
/// light) and their light; the lights of the moment; for a posed model, its first bone in
/// the frame's bones; and the fog ([`gpu::fog_params`]).
fn camera_bytes(
    matrix: [f32; 16],
    eye: [f32; 4],
    light: Option<&gmod_world::light::ModelLight>,
    moment: &[f32],
    first_bone: Option<usize>,
    fog: &[f32; 8],
) -> Vec<u8> {
    let mut values: Vec<f32> = matrix.iter().chain(&eye).copied().collect();
    match light {
        Some(light) => {
            for (i, side) in light.ambient.iter().enumerate() {
                values.extend(side);
                values.push(if i == 0 { 1.0 } else { 0.0 });
            }
            for i in 0..gmod_world::light::MAX_LIGHTS {
                match light.lights.get(i) {
                    Some((towards, _)) => values.extend([towards[0], towards[1], towards[2], 1.0]),
                    None => values.extend([0.0; 4]),
                }
            }
            for i in 0..gmod_world::light::MAX_LIGHTS {
                let color = light.lights.get(i).map_or([0.0; 3], |(_, c)| *c);
                values.extend([color[0], color[1], color[2], 0.0]);
            }
        }
        None => values.resize(values.len() + 4 * (6 + 2 * gmod_world::light::MAX_LIGHTS), 0.0),
    }
    values.extend(moment);
    // the lights of the moment take 4 x (2 x 4 + 3) floats, also when there are none
    values.resize(4 * (1 + 4 + 6 + 2 * gmod_world::light::MAX_LIGHTS + 2 * MAX_POINT_LIGHTS + 3), 0.0);
    values.extend(match first_bone {
        Some(bone) => [bone as f32, 1.0, 0.0, 0.0],
        None => [0.0; 4],
    });
    values.extend(fog);
    let mut bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
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
    bones: wgpu::BindGroupLayout,
}

/// A buffer for the bones of `room` matrices, and its bind group.
fn bones_buffer(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, room: usize) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("bones"),
        size: room.max(1) as u64 * BONE_SIZE,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bones"),
        layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
    });
    (buffer, group)
}

/// What uploading needs after the world is up: the layouts, the samplers, the missing texture
/// and the white light map of what is lit per vertex.
struct Shared {
    layouts: Layouts,
    samplers: Vec<wgpu::Sampler>,
    missing: Views,
    white: wgpu::TextureView,
    lightmap_sampler: wgpu::Sampler,
    compressed: bool,
}

impl Shared {
    fn texture(&self, device: &wgpu::Device, queue: &wgpu::Queue, texture: &Texture) -> Uploaded {
        let data = gpu::texture_data(&texture.vtf, self.compressed, MAX_TEXTURE)?;
        Some(views(device, queue, &texture.name, &data))
    }

    /// Uploads pieces of a world or a model, whose textures are `textures`.
    fn mesh(&self, device: &wgpu::Device, queue: &wgpu::Queue, pieces: &[Piece], textures: &[Uploaded]) -> Mesh {
        let texture = |index: Option<usize>| pick(textures, &self.missing, index);
        let geometry = gpu::geometry(pieces);
        let vertices = buffer(device, queue, "vertices", wgpu::BufferUsages::VERTEX, &geometry.vertices);
        let indices = buffer(device, queue, "indices", wgpu::BufferUsages::INDEX, &geometry.indices);
        let mut params = vec![0u8; (geometry.draws.len() as u64 * PARAMS_STRIDE).max(PARAMS_STRIDE) as usize];
        let mut draws = Vec::with_capacity(geometry.draws.len());
        let mut animated = Vec::new();
        for (i, draw) in geometry.draws.iter().enumerate() {
            let piece = &pieces[draw.piece];
            let surface = &piece.surfaces[draw.surface];
            let (frames, lightmap) = (texture(surface.base).frames, piece.lightmap.is_some());
            let values = gpu::surface_params(surface, &surface.look(0.0), frames, lightmap);
            let at = i * PARAMS_STRIDE as usize;
            for (k, v) in values.iter().enumerate() {
                params[at + k * 4..at + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            if surface.proxies.is_some() {
                animated.push(Animated { draw: i, surface: surface.clone(), frames, lightmap });
            }
            let kind = match (surface.translucent, surface.additive, surface.no_cull) {
                _ if surface.modulate => Kind::Modulate,
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
                let surface = &pieces[draw.piece].surfaces[draw.surface];
                let base = texture(surface.base);
                let blend = texture(surface.base2.or(surface.base));
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
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&base.layers) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&blend.layers) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.samplers[base.sampler]) },
                    ],
                })
            })
            .collect();
        // light maps; what is lit per vertex gets the white one
        let lightmap_groups = pieces
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
        // a model that moves takes the bones of its vertices along
        let bone_count = gpu::bone_count(pieces);
        let skin = (bone_count > 0).then(|| (buffer(device, queue, "skin", wgpu::BufferUsages::VERTEX, &gpu::skin(pieces)), bone_count));
        Mesh { vertices, indices, draws, surface_groups, lightmap_groups, skin, params: params_buffer, animated }
    }
}

/// Uploads a texture's pixels: its views, its sampler by how it clamps, its frames.
fn views(device: &wgpu::Device, queue: &wgpu::Queue, label: &str, data: &TextureData) -> Views {
    let (layers, first) = upload_texture(device, queue, label, data);
    Views { layers, first, sampler: usize::from(data.clamp_s) | usize::from(data.clamp_t) << 1, frames: data.frames.max(1) }
}

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    kind: Kind,
    posed: bool,
) -> wgpu::RenderPipeline {
    let attributes = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 1 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 32, shader_location: 2 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32, offset: 40, shader_location: 3 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 44, shader_location: 4 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 5 },
    ];
    // the bones of each vertex of a posed model: three numbers, then their weights
    let skin_attributes = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint8x4, offset: 0, shader_location: 6 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 4, shader_location: 7 },
    ];
    let vertices = Some(wgpu::VertexBufferLayout {
        array_stride: gpu::VERTEX_SIZE as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attributes,
    });
    let skin = Some(wgpu::VertexBufferLayout {
        array_stride: gpu::SKIN_SIZE as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &skin_attributes,
    });
    let buffers = if posed { vec![vertices, skin] } else { vec![vertices] };
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
    // what is behind times twice the colour: the shader gives half of it
    let modulate = wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Dst,
            dst_factor: wgpu::BlendFactor::Src,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: additive.alpha,
    };
    let blend = match kind {
        Kind::Opaque | Kind::OpaqueTwoSided => None,
        Kind::Blend | Kind::BlendTwoSided => Some(wgpu::BlendState::ALPHA_BLENDING),
        Kind::Additive => Some(additive),
        Kind::Modulate => Some(modulate),
    };
    let two_sided = matches!(kind, Kind::OpaqueTwoSided | Kind::BlendTwoSided | Kind::Additive);
    let targets = [Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if posed { "posed" } else { "world" }),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(if posed { "vs_posed" } else { "vs_main" }),
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
                entries: &[
                    // the vertex shader moves the texture coordinates
                    uniform_entry(0, wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT),
                    view_entry(1, wgpu::TextureViewDimension::D2Array),
                    view_entry(2, wgpu::TextureViewDimension::D2Array),
                    sampler_entry(3),
                ],
            }),
            texture: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("texture"),
                entries: &[texture_entry(0), sampler_entry(1)],
            }),
            bones: device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bones"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(BONE_SIZE),
                    },
                    count: None,
                }],
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
        let posed_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("posed"),
            bind_group_layouts: &[Some(&layouts.camera), Some(&layouts.surface), Some(&layouts.texture), Some(&layouts.bones)],
            immediate_size: 0,
        });
        let pipelines = Pipelines {
            still: KINDS.iter().map(|&kind| pipeline(device, &world_layout, &shader, format, kind, false)).collect(),
            posed: KINDS.iter().map(|&kind| pipeline(device, &posed_layout, &shader, format, kind, true)).collect(),
            sky: sky_pipeline(device, &layouts, format),
        };
        let bones_room = 256;
        let (bones_buffer, bones_group) = bones_buffer(device, &layouts.bones, bones_room);

        let camera_slots = 1;
        let sky_camera_buffer = camera_buffer(device, 2);
        let sky_camera_group = camera_group(device, &layouts.camera, &sky_camera_buffer);
        let camera_buffer = camera_buffer(device, camera_slots);
        let camera_group = camera_group(device, &layouts.camera, &camera_buffer);

        let shared = Shared {
            samplers: (0..4).map(|i| sampler(device, i & 1 != 0, i & 2 != 0, true)).collect(),
            missing: views(device, queue, "missing", &gpu::missing_texture()),
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
        let mesh = shared.mesh(device, queue, &world.pieces, &textures);
        let skybox = world.skybox.as_ref().map(|skybox| (shared.mesh(device, queue, &skybox.pieces, &textures), skybox.camera));

        // the sky box: two triangles a face
        let quads = gpu::sky_quads(world);
        let sky_sampler = sampler(device, true, true, false);
        let mut sky_bytes = Vec::new();
        let mut sky_groups = Vec::new();
        for quad in &quads {
            for corner in [0, 1, 2, 0, 2, 3] {
                sky_bytes.extend(quad.corners[corner].iter().flat_map(|v| v.to_le_bytes()));
            }
            let view = &pick(&textures, &shared.missing, Some(quad.texture)).first;
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
            skybox,
            fog: gpu::fog_params(world.fog.as_ref()),
            models: Vec::new(),
            textures: by_name,
            entities: Vec::new(),
            points: Vec::new(),
            spot: None,
            view_models: Vec::new(),
            view_model_fov: 54.0,
            bones_buffer,
            bones_group,
            bones_room,
            sky_vertices,
            sky_groups,
            depth: None,
            time: 0.0,
            failed_textures,
        }
    }

    /// The time from now on, in seconds into the map: what materials that change with time
    /// show (their proxies' `CurrentTime`).
    pub fn set_time(&mut self, seconds: f32) {
        self.time = seconds;
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
        self.models.push(self.shared.mesh(device, queue, &model.pieces, &textures));
        ModelId(self.models.len() - 1)
    }

    /// The models to draw from now on, and where.
    /// The lights of the moment from now on: points (the first [`MAX_POINT_LIGHTS`] that
    /// light each kind of thing) and a cone.
    pub fn set_lights(&mut self, points: Vec<PointLight>, spot: Option<SpotLight>) {
        self.points = points;
        self.spot = spot;
    }

    pub fn set_entities(&mut self, entities: Vec<Placed>) {
        self.entities = entities.into_iter().filter(|e| e.model.0 < self.models.len()).collect();
    }

    /// The view models to draw from now on - the weapon in the player's hands and the hands -
    /// seen from the same eye with their own field of view (degrees), in front of everything.
    pub fn set_view_models(&mut self, view_models: Vec<Placed>, fov: f32) {
        self.view_models = view_models.into_iter().filter(|e| e.model.0 < self.models.len()).collect();
        self.view_model_fov = fov;
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
        let held = Camera { fov: self.view_model_fov, ..*camera }.view_projection_near(aspect, VIEW_MODEL_NEAR);
        // the world's camera, then each entity's and each view model's: the camera after its
        // placement
        let slots = 1 + self.entities.len() + self.view_models.len();
        if slots > self.camera_slots {
            self.camera_slots = slots.next_power_of_two();
            self.camera_buffer = camera_buffer(device, self.camera_slots);
            self.camera_group = camera_group(device, &self.shared.layouts.camera, &self.camera_buffer);
        }
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let world_lights = lights_bytes(&self.points, self.spot.as_ref(), false, [0.0; 3], identity, 1.0);
        let [x, y, z] = camera.eye;
        let mut cameras = camera_bytes(view_projection, [x, y, z, 1.0], None, &world_lights, None, &self.fog);
        // the bones of the posed models, one after another
        let mut bones: Vec<u8> = Vec::new();
        let placements = self.entities.iter().map(|p| (p, &view_projection)).chain(self.view_models.iter().map(|p| (p, &held)));
        for (placed, projection) in placements {
            let (mut matrix, _) = gpu::placement(placed.origin, placed.angles);
            let axes = gmod_world::axes(placed.angles);
            let light = gmod_world::light::in_model_space(&placed.light, axes);
            let moment = lights_bytes(&self.points, self.spot.as_ref(), true, placed.origin, axes, placed.scale);
            matrix[..12].iter_mut().for_each(|v| *v *= placed.scale);
            // a model is posed when it is given the bones its vertices name
            let first_bone = match &self.models[placed.model.0].skin {
                Some((_, needed)) if placed.bones.len() >= *needed => {
                    let first = bones.len() / BONE_SIZE as usize;
                    bones.extend(placed.bones.iter().flatten().flatten().flat_map(|v| v.to_le_bytes()));
                    Some(first)
                }
                _ => None,
            };
            let [x, y, z] = model_space(camera.eye, placed.origin, axes, placed.scale);
            let eye = [x, y, z, placed.scale];
            cameras.extend(camera_bytes(gpu::multiply(projection, &matrix), eye, Some(&light), &moment, first_bone, &self.fog));
        }
        queue.write_buffer(&self.camera_buffer, 0, &cameras);
        let bone_count = bones.len() / BONE_SIZE as usize;
        if bone_count > self.bones_room {
            self.bones_room = bone_count.next_power_of_two();
            (self.bones_buffer, self.bones_group) = bones_buffer(device, &self.shared.layouts.bones, self.bones_room);
        }
        if !bones.is_empty() {
            queue.write_buffer(&self.bones_buffer, 0, &bones);
        }
        // the sky box's camera, then the 3D sky box's: from the sky camera, with its fog and
        // none of the lights of the moment, which light the world
        let mut sky_cameras = camera_bytes(camera.sky_projection(aspect), [0.0, 0.0, 0.0, 1.0], None, &[], None, &[0.0; 8]);
        let sky_eye = self.skybox.as_ref().map(|(_, sky)| sky.eye(camera.eye));
        if let (Some((_, sky)), Some(eye)) = (&self.skybox, sky_eye) {
            // what is nearer than in the world, as the world counts it, is cut off
            let projection = Camera { eye, ..*camera }.view_projection_near(aspect, gpu::NEAR / sky.scale);
            // its fog is counted in the world's units, `scale` to a unit of the sky box
            let fog = gpu::fog_params(sky.fog.as_ref());
            sky_cameras.extend(camera_bytes(projection, [eye[0], eye[1], eye[2], sky.scale], None, &[], None, &fog));
        }
        queue.write_buffer(&self.sky_camera_buffer, 0, &sky_cameras);
        // the surfaces that change with time, as they are now
        let meshes = std::iter::once(&self.world).chain(self.skybox.as_ref().map(|(mesh, _)| mesh)).chain(&self.models);
        for mesh in meshes {
            mesh.animate(queue, self.time);
        }

        // the sky: the sky box, then the 3D sky box
        let clear = wgpu::LoadOp::Clear(wgpu::Color { r: 0.25, g: 0.35, b: 0.5, a: 1.0 });
        let mut pass = begin_pass(encoder, "sky", target, &depth, clear);
        if let Some(sky) = &self.sky_vertices {
            pass.set_pipeline(&self.pipelines.sky);
            pass.set_bind_group(0, &self.sky_camera_group, &[0]);
            pass.set_vertex_buffer(0, sky.slice(..));
            for (i, group) in self.sky_groups.iter().enumerate() {
                pass.set_bind_group(1, group, &[]);
                pass.draw(i as u32 * 6..i as u32 * 6 + 6, 0..1);
            }
        }
        if let (Some((mesh, _)), Some(eye)) = (&self.skybox, sky_eye) {
            let draw = Drawing { pipelines: &self.pipelines, camera: &self.sky_camera_group, bones: &self.bones_group };
            let (opaque, translucent) = order(&mesh.draws, eye);
            let mut current = None;
            draw.mesh(&mut pass, &mut current, CAMERA_STRIDE as u32, mesh, opaque);
            draw.mesh(&mut pass, &mut current, CAMERA_STRIDE as u32, mesh, translucent);
        }
        drop(pass);

        // the world, its depth cleared: all of the sky is behind it, however near the 3D sky
        // box was drawn
        let mut pass = begin_pass(encoder, "world", target, &depth, wgpu::LoadOp::Load);
        let (opaque, translucent) = order(&self.world.draws, camera.eye);
        let draw = Drawing { pipelines: &self.pipelines, camera: &self.camera_group, bones: &self.bones_group };
        let mut current = None;
        draw.mesh(&mut pass, &mut current, 0, &self.world, opaque);
        let slot = |slot: usize| (slot as u64 * CAMERA_STRIDE) as u32;
        for (i, placed) in self.entities.iter().enumerate() {
            let mesh = &self.models[placed.model.0];
            draw.mesh(&mut pass, &mut current, slot(1 + i), mesh, (0..mesh.draws.len()).filter(|&i| !mesh.draws[i].1.translucent()));
        }
        draw.mesh(&mut pass, &mut current, 0, &self.world, translucent);
        for (i, placed) in self.entities.iter().enumerate() {
            let mesh = &self.models[placed.model.0];
            draw.mesh(&mut pass, &mut current, slot(1 + i), mesh, (0..mesh.draws.len()).filter(|&i| mesh.draws[i].1.translucent()));
        }
        drop(pass);
        if self.view_models.is_empty() {
            return;
        }
        // the view models over everything: what they hide of the world they hide, however near
        // a wall is
        let mut pass = begin_pass(encoder, "view models", target, &depth, wgpu::LoadOp::Load);
        let mut current = None;
        let first = 1 + self.entities.len();
        for translucent in [false, true] {
            for (i, placed) in self.view_models.iter().enumerate() {
                let mesh = &self.models[placed.model.0];
                let list = (0..mesh.draws.len()).filter(|&i| mesh.draws[i].1.translucent() == translucent);
                draw.mesh(&mut pass, &mut current, slot(first + i), mesh, list);
            }
        }
    }
}

/// A pass drawing into `target`, its colour loaded or cleared, its depth cleared.
fn begin_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    depth: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: depth,
            depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// The order to draw a mesh's surfaces seen from `eye`: the opaque ones by pipeline, to
/// switch less, and the translucent ones from the farthest.
fn order(draws: &[(Draw, Kind)], eye: [f32; 3]) -> (Vec<usize>, Vec<usize>) {
    let distance = |draw: &Draw| {
        let d = [draw.center[0] - eye[0], draw.center[1] - eye[1], draw.center[2] - eye[2]];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    };
    let mut opaque: Vec<usize> = (0..draws.len()).filter(|&i| !draws[i].1.translucent()).collect();
    opaque.sort_by_key(|&i| draws[i].1 as u8);
    let mut translucent: Vec<usize> = (0..draws.len()).filter(|&i| draws[i].1.translucent()).collect();
    translucent.sort_by(|&a, &b| distance(&draws[b].0).total_cmp(&distance(&draws[a].0)));
    (opaque, translucent)
}

/// What every draw of a frame shares: the pipelines, the cameras and the bones.
struct Drawing<'a> {
    pipelines: &'a Pipelines,
    camera: &'a wgpu::BindGroup,
    bones: &'a wgpu::BindGroup,
}

impl Drawing<'_> {
    /// Draws the surfaces `list` of a mesh, seen through the camera at `offset`; `current` is
    /// the pipeline set last.
    fn mesh(
        &self,
        pass: &mut wgpu::RenderPass,
        current: &mut Option<(Kind, bool)>,
        offset: u32,
        mesh: &Mesh,
        list: impl IntoIterator<Item = usize>,
    ) {
        pass.set_bind_group(0, self.camera, &[offset]);
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
        let posed = mesh.skin.is_some();
        if let Some((skin, _)) = &mesh.skin {
            pass.set_vertex_buffer(1, skin.slice(..));
            pass.set_bind_group(3, self.bones, &[]);
        }
        for i in list {
            let (draw, kind) = mesh.draws[i];
            if draw.index_count == 0 {
                continue;
            }
            if *current != Some((kind, posed)) {
                pass.set_pipeline(self.pipelines.get(kind, posed));
                *current = Some((kind, posed));
            }
            pass.set_bind_group(1, &mesh.surface_groups[i], &[]);
            pass.set_bind_group(2, &mesh.lightmap_groups[draw.piece], &[]);
            pass.draw_indexed(draw.first_index..draw.first_index + draw.index_count, 0, 0..1);
        }
    }
}

#[cfg(test)]
mod tests;
