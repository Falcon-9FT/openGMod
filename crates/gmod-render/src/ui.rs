//! The 2D over the 3D view: the HUD and the panels, as a [`gmod_ui::Canvas`] has them -
//! triangles in batches, each with its texture (plain colour, a page of the font atlas, an
//! image), its blending (covering, or adding light) and the rectangle it is cut to.
//!
//! The font atlas's pages go to the GPU when they change ([`Overlay::update_glyphs`]); images
//! are given by number, once, by whoever can read them ([`Overlay::missing_images`],
//! [`Overlay::set_image`]): an image that is not there is not drawn.

use std::collections::HashMap;

use gmod_ui::{Canvas, Fonts, Texture, Vertex};

/// Bytes of the screen's uniform: its size, whether the target is sRGB, padding.
const SCREEN_SIZE: u64 = 16;

/// Draws a frame's 2D.
pub struct Overlay {
    srgb: bool,
    blend: wgpu::RenderPipeline,
    additive: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    screen: wgpu::Buffer,
    screen_group: wgpu::BindGroup,
    white: wgpu::BindGroup,
    pages: Vec<(wgpu::Texture, wgpu::BindGroup)>,
    /// Images by number; none for one that could not be read.
    images: HashMap<u32, Option<wgpu::BindGroup>>,
    /// The textures of images that change ([`Overlay::update_image`]), with their sizes.
    live: HashMap<u32, (wgpu::Texture, u32, u32)>,
    vertices: wgpu::Buffer,
}

fn pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    additive: bool,
) -> wgpu::RenderPipeline {
    let attributes = [
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 8, shader_location: 1 },
        wgpu::VertexAttribute { format: wgpu::VertexFormat::Unorm8x4, offset: 16, shader_location: 2 },
    ];
    let color = wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::SrcAlpha,
        dst_factor: if additive { wgpu::BlendFactor::One } else { wgpu::BlendFactor::OneMinusSrcAlpha },
        operation: wgpu::BlendOperation::Add,
    };
    let alpha = wgpu::BlendComponent::OVER;
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(if additive { "ui additive" } else { "ui" }),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: Vertex::SIZE as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            })],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState { color, alpha }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

impl Overlay {
    /// For targets of `format`.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Overlay {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ui.wgsl").into()),
        });
        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui screen"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(SCREEN_SIZE),
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui"),
            bind_group_layouts: &[Some(&screen_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let screen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui screen"),
            size: SCREEN_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui screen"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: screen.as_entire_binding() }],
        });
        Overlay {
            srgb: format.is_srgb(),
            blend: pipeline(device, &shader, &layout, format, false),
            additive: pipeline(device, &shader, &layout, format, true),
            white: device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ui white"),
                layout: &texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&upload(device, queue, 1, 1, &[255; 4], false).1),
                    },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                ],
            }),
            texture_layout,
            sampler,
            screen,
            screen_group,
            pages: Vec::new(),
            images: HashMap::new(),
            live: HashMap::new(),
            vertices: vertex_buffer(device, 4096 * Vertex::SIZE as u64),
        }
    }

    fn group(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui texture"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// Sends the pages of the font atlas that changed.
    pub fn update_glyphs(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, fonts: &mut Fonts) {
        for page in fonts.take_changed() {
            let pixels = &fonts.pages()[page as usize].pixels;
            let side = gmod_ui::font::PAGE_SIZE;
            while self.pages.len() <= page as usize {
                let (texture, view) = upload(device, queue, side, side, &vec![0; (side * side * 4) as usize], false);
                let group = self.group(device, &view);
                self.pages.push((texture, group));
            }
            write(queue, &self.pages[page as usize].0, side, side, pixels);
        }
    }

    /// The images the canvas draws that have not been given yet.
    pub fn missing_images(&self, canvas: &Canvas) -> Vec<u32> {
        let mut missing: Vec<u32> = canvas
            .batches()
            .iter()
            .filter_map(|b| match b.texture {
                Texture::Image(id) if !self.images.contains_key(&id) => Some(id),
                _ => None,
            })
            .collect();
        missing.sort_unstable();
        missing.dedup();
        missing
    }

    /// An image by number: its RGBA pixels, `width` by `height`, rows top to bottom - or none
    /// when it could not be read, and is not drawn.
    pub fn set_image(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: u32, image: Option<(u32, u32, &[u8])>) {
        let group = image.filter(|(w, h, pixels)| *w > 0 && *h > 0 && pixels.len() as u64 >= u64::from(*w) * u64::from(*h) * 4).map(
            |(width, height, pixels)| {
                let limit = device.limits().max_texture_dimension_2d;
                let (width, height, pixels) = shrink(width, height, pixels, limit);
                let (_, view) = upload(device, queue, width, height, &pixels, self.srgb);
                self.group(device, &view)
            },
        );
        self.images.insert(id, group);
    }

    /// An image by number that changes from frame to frame (a web page): its new pixels, RGBA
    /// `width` by `height`, go into its texture, which is made again only when the size changed.
    pub fn update_image(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: u32, width: u32, height: u32, pixels: &[u8]) {
        let limit = device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > limit || height > limit || (pixels.len() as u64) < u64::from(width) * u64::from(height) * 4
        {
            return;
        }
        match self.live.get(&id) {
            Some((texture, w, h)) if (*w, *h) == (width, height) => write(queue, texture, width, height, pixels),
            _ => {
                let (texture, view) = upload(device, queue, width, height, pixels, self.srgb);
                let group = self.group(device, &view);
                self.images.insert(id, Some(group));
                self.live.insert(id, (texture, width, height));
            }
        }
    }

    /// Draws the canvas over what `target` (`size` pixels) has.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: (u32, u32),
        canvas: &Canvas,
    ) {
        if canvas.is_empty() || size.0 == 0 || size.1 == 0 {
            return;
        }
        let mut screen = Vec::with_capacity(SCREEN_SIZE as usize);
        for value in [canvas.width.max(1) as f32, canvas.height.max(1) as f32, if self.srgb { 1.0 } else { 0.0 }, 0.0] {
            screen.extend_from_slice(&value.to_le_bytes());
        }
        queue.write_buffer(&self.screen, 0, &screen);
        let mut bytes = Vec::with_capacity(canvas.vertices().len() * Vertex::SIZE);
        for vertex in canvas.vertices() {
            vertex.bytes(&mut bytes);
        }
        if bytes.len() as u64 > self.vertices.size() {
            self.vertices = vertex_buffer(device, (bytes.len() as u64).next_power_of_two());
        }
        queue.write_buffer(&self.vertices, 0, &bytes);

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("ui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.screen_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        let mut additive = None;
        for batch in canvas.batches() {
            let group = match batch.texture {
                Texture::White => Some(&self.white),
                Texture::Glyphs(page) => self.pages.get(page as usize).map(|(_, group)| group),
                Texture::Image(id) => self.images.get(&id).and_then(Option::as_ref),
            };
            let Some(group) = group else { continue };
            // the canvas is in the screen's pixels; the target may be another size
            let scale = (size.0 as f32 / canvas.width.max(1) as f32, size.1 as f32 / canvas.height.max(1) as f32);
            let (x, y, w, h) = match batch.clip {
                Some(clip) => {
                    let left = (clip.left as f32 * scale.0).round().clamp(0.0, size.0 as f32) as u32;
                    let top = (clip.top as f32 * scale.1).round().clamp(0.0, size.1 as f32) as u32;
                    let right = (clip.right as f32 * scale.0).round().clamp(0.0, size.0 as f32) as u32;
                    let bottom = (clip.bottom as f32 * scale.1).round().clamp(0.0, size.1 as f32) as u32;
                    (left, top, right.saturating_sub(left), bottom.saturating_sub(top))
                }
                None => (0, 0, size.0, size.1),
            };
            if w == 0 || h == 0 {
                continue;
            }
            pass.set_scissor_rect(x, y, w, h);
            if additive != Some(batch.additive) {
                pass.set_pipeline(if batch.additive { &self.additive } else { &self.blend });
                additive = Some(batch.additive);
            }
            pass.set_bind_group(1, group, &[]);
            pass.draw(batch.start..batch.start + batch.count, 0..1);
        }
    }
}

fn vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ui vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// A texture of RGBA pixels, sRGB or as they are.
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    pixels: &[u8],
    srgb: bool,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ui image"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if srgb { wgpu::TextureFormat::Rgba8UnormSrgb } else { wgpu::TextureFormat::Rgba8Unorm },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    write(queue, &texture, width, height, pixels);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, width: u32, height: u32, pixels: &[u8]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &pixels[..(width * height * 4) as usize],
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}

/// An image no bigger than `limit` on a side, halved as often as it has to be.
fn shrink(width: u32, height: u32, pixels: &[u8], limit: u32) -> (u32, u32, Vec<u8>) {
    let (mut width, mut height, mut pixels) = (width, height, pixels[..(width * height * 4) as usize].to_vec());
    while width > limit || height > limit {
        let (w, h) = ((width / 2).max(1), (height / 2).max(1));
        let mut half = vec![0; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let from = (((y * 2).min(height - 1) * width + (x * 2).min(width - 1)) * 4) as usize;
                let to = ((y * w + x) * 4) as usize;
                half[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
            }
        }
        (width, height, pixels) = (w, h, half);
    }
    (width, height, pixels)
}
