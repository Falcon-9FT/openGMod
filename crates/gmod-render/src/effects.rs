//! What the game draws in the world frame by frame - the physics gun's beam, and the beams,
//! sprites and quads Lua draws (`render.DrawBeam` ...): triangles in world units, each corner
//! with a texture coordinate and a colour, in batches by texture and by whether they blend over
//! what is behind them or add to it. They are drawn after the world and the entities, seen
//! through: hidden by what is in front of them, hiding nothing.

use std::collections::HashMap;

use crate::{DEPTH_FORMAT, Views};

/// A corner of an effect's triangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectVertex {
    /// In the world.
    pub position: [f32; 3],
    pub uv: [f32; 2],
    /// Red, green, blue (sRGB) and alpha, from 0 to 1.
    pub color: [f32; 4],
}

const VERTEX_SIZE: u64 = 36;

/// Triangles with one texture, three corners each.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectBatch {
    /// A texture uploaded by name ([`crate::Renderer::add_effect_texture`]); none: plain white,
    /// the colours alone.
    pub texture: Option<String>,
    /// Added to what is behind it, not blended over it.
    pub additive: bool,
    pub vertices: Vec<EffectVertex>,
}

/// The effects of the frame, and what draws them.
pub(crate) struct Effects {
    layout: wgpu::BindGroupLayout,
    blend: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    white: Views,
    groups: HashMap<Option<String>, wgpu::BindGroup>,
    buffer: Option<(wgpu::Buffer, u64)>,
    pub batches: Vec<EffectBatch>,
}

impl Effects {
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue, camera: &wgpu::BindGroupLayout, format: wgpu::TextureFormat) -> Effects {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effects"),
            source: wgpu::ShaderSource::Wgsl(include_str!("effects.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects"),
            bind_group_layouts: &[Some(camera), Some(&layout)],
            immediate_size: 0,
        });
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
        let make = |label: &str, blend: wgpu::BlendState| {
            let attributes = [
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 12, shader_location: 1 },
                wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 20, shader_location: 2 },
            ];
            let buffers = [Some(wgpu::VertexBufferLayout {
                array_stride: VERTEX_SIZE,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })];
            let targets = [Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &buffers,
                },
                // both sides: a beam turned to the eye shows whichever way its corners go round
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
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
        };
        let white = crate::views(device, queue, "white", &white_texture());
        Effects {
            layout,
            blend: make("effects blended", wgpu::BlendState::ALPHA_BLENDING),
            additive: make("effects added", additive),
            white,
            groups: HashMap::new(),
            buffer: None,
            batches: Vec::new(),
        }
    }

    /// The vertices of the frame on the GPU, and a bind group for each texture used; then the
    /// batches, each a range of the vertices.
    /// A texture made again: what was bound of it before goes.
    pub(crate) fn forget(&mut self, name: &str) {
        self.groups.remove(&Some(name.to_string()));
    }

    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures: &HashMap<String, crate::Uploaded>,
        samplers: &[wgpu::Sampler],
    ) -> Vec<(Option<String>, bool, std::ops::Range<u32>)> {
        let mut bytes = Vec::new();
        let mut ranges = Vec::with_capacity(self.batches.len());
        for batch in &self.batches {
            let first = (bytes.len() as u64 / VERTEX_SIZE) as u32;
            for v in &batch.vertices {
                for x in v.position.iter().chain(&v.uv).chain(&v.color) {
                    bytes.extend(x.to_le_bytes());
                }
            }
            let last = (bytes.len() as u64 / VERTEX_SIZE) as u32;
            let key = batch.texture.clone().filter(|name| matches!(textures.get(name), Some(Some(_))));
            if !self.groups.contains_key(&key) {
                let views = key.as_ref().and_then(|name| textures.get(name).cloned().flatten()).unwrap_or_else(|| self.white.clone());
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("effect texture"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views.layers) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&samplers[views.sampler]) },
                    ],
                });
                self.groups.insert(key.clone(), group);
            }
            ranges.push((key, batch.additive, first..last));
        }
        if bytes.is_empty() {
            return Vec::new();
        }
        let size = (bytes.len() as u64).next_power_of_two().max(4096);
        if self.buffer.as_ref().is_none_or(|(_, room)| *room < bytes.len() as u64) {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("effects"),
                size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.buffer = Some((buffer, size));
        }
        if let Some((buffer, _)) = &self.buffer {
            queue.write_buffer(buffer, 0, &bytes);
        }
        ranges
    }

    /// Draws the batches prepared, with the world's camera.
    pub(crate) fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        camera: &wgpu::BindGroup,
        ranges: &[(Option<String>, bool, std::ops::Range<u32>)],
    ) {
        let Some((buffer, _)) = &self.buffer else { return };
        if ranges.is_empty() {
            return;
        }
        pass.set_bind_group(0, camera, &[0]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        for (key, additive, range) in ranges {
            let Some(group) = self.groups.get(key) else { continue };
            pass.set_pipeline(if *additive { &self.additive } else { &self.blend });
            pass.set_bind_group(1, group, &[]);
            pass.draw(range.clone(), 0..1);
        }
    }
}

/// One white texel: what plain colours are drawn with.
fn white_texture() -> gmod_world::gpu::TextureData {
    gmod_world::gpu::TextureData {
        width: 1,
        height: 1,
        format: gmod_world::gpu::PixelFormat::Rgba8,
        frames: 1,
        levels: vec![vec![255, 255, 255, 255]],
        clamp_s: false,
        clamp_t: false,
    }
}
