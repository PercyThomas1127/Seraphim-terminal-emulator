//! Inline images (Kitty graphics, Sixel): one textured quad per visible
//! placement, in three z-layers around the cell passes:
//! below cell backgrounds (z < -2^30), between backgrounds and text
//! (z < 0), and above text (z >= 0), as the kitty protocol specifies.

use std::collections::{HashMap, HashSet};

use seraphim_terminal::graphics::BELOW_BACKGROUND;
use seraphim_terminal::Terminal;

#[repr(C)]
#[derive(Clone, Copy)]
struct Quad {
    rect: [f32; 4],
    uv: [f32; 4],
}
// SAFETY: plain f32 arrays, repr(C), no padding.
unsafe impl bytemuck::Zeroable for Quad {}
unsafe impl bytemuck::Pod for Quad {}

struct Texture {
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
}

/// A run of quads that share a texture, in one layer.
struct Draw {
    key: u64,
    layer: usize,
    range: std::ops::Range<u32>,
}

pub(crate) struct Images {
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    globals_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    textures: HashMap<u64, Texture>,
    buffer: wgpu::Buffer,
    capacity: usize,
    draws: Vec<Draw>,
    max_side: u32,
}

/// Which layer a z-index belongs to.
fn layer(z: i32) -> usize {
    if z < BELOW_BACKGROUND {
        0
    } else if z < 0 {
        1
    } else {
        2
    }
}

impl Images {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, globals: &wgpu::Buffer) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("images"),
            source: wgpu::ShaderSource::Wgsl(include_str!("image.wgsl").into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image texture"),
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
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("images"),
            bind_group_layouts: &[Some(&globals_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("images"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let capacity = 64;
        Images {
            pipeline,
            texture_layout,
            globals_group,
            sampler,
            textures: HashMap::new(),
            buffer: Self::make_buffer(device, capacity),
            capacity,
            draws: Vec::new(),
            max_side: device.limits().max_texture_dimension_2d,
        }
    }

    fn make_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image quads"),
            size: (capacity * std::mem::size_of::<Quad>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Build this frame's quads and upload any new images.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        term: &Terminal,
        pad: f32,
        cell: (f32, f32),
    ) {
        self.draws.clear();
        let visible = term.visible_images();
        // Drop textures of images the terminal no longer has.
        if !self.textures.is_empty() {
            let alive: HashSet<u64> = term.graphics().images().map(|i| i.key).collect();
            self.textures.retain(|k, _| alive.contains(k));
        }
        if visible.is_empty() {
            return;
        }
        let (cw, ch) = cell;
        let mut quads: Vec<(usize, u64, Quad)> = Vec::with_capacity(visible.len());
        for v in &visible {
            let p = v.placement;
            let img = v.image;
            if !self.textures.contains_key(&img.key) {
                let t = self.upload(device, queue, img.width, img.height, &img.rgba);
                self.textures.insert(img.key, t);
            }
            let x = (pad + p.col as f32 * cw + p.offset[0] as f32).round();
            let y = (pad + v.row as f32 * ch + p.offset[1] as f32).round();
            let (w, h) = if p.fill {
                (
                    (p.cols as f32 * cw - p.offset[0] as f32).max(1.0),
                    (p.rows as f32 * ch - p.offset[1] as f32).max(1.0),
                )
            } else {
                (p.src[2] as f32, p.src[3] as f32)
            };
            let (iw, ih) = (img.width.max(1) as f32, img.height.max(1) as f32);
            let uv = [
                p.src[0] as f32 / iw,
                p.src[1] as f32 / ih,
                p.src[2] as f32 / iw,
                p.src[3] as f32 / ih,
            ];
            quads.push((layer(p.z), img.key, Quad { rect: [x, y, w, h], uv }));
        }
        // Stable: within a layer, z order (already sorted) is kept.
        quads.sort_by_key(|q| q.0);
        if quads.len() > self.capacity {
            self.capacity = quads.len().next_power_of_two();
            self.buffer = Self::make_buffer(device, self.capacity);
        }
        let data: Vec<Quad> = quads.iter().map(|q| q.2).collect();
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&data));
        for (i, (layer, key, _)) in quads.iter().enumerate() {
            let i = i as u32;
            match self.draws.last_mut() {
                Some(d) if d.key == *key && d.layer == *layer && d.range.end == i => {
                    d.range.end += 1
                }
                _ => self.draws.push(Draw { key: *key, layer: *layer, range: i..i + 1 }),
            }
        }
    }

    fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Texture {
        // Larger than the GPU allows: shrink (nearest) to fit.
        let (w2, h2, data) = if w > self.max_side || h > self.max_side {
            let s = self.max_side as f32 / w.max(h) as f32;
            let (nw, nh) = (((w as f32 * s) as u32).max(1), ((h as f32 * s) as u32).max(1));
            let mut out = Vec::with_capacity((nw * nh * 4) as usize);
            for y in 0..nh {
                let sy = (y as f32 / s) as u32;
                for x in 0..nw {
                    let sx = (x as f32 / s) as u32;
                    let i = ((sy.min(h - 1) * w + sx.min(w - 1)) * 4) as usize;
                    out.extend_from_slice(&rgba[i..i + 4]);
                }
            }
            (nw, nh, std::borrow::Cow::Owned(out))
        } else {
            (w, h, std::borrow::Cow::Borrowed(rgba))
        };
        let size = wgpu::Extent3d { width: w2, height: h2, depth_or_array_layers: 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("inline image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * w2),
                rows_per_image: Some(h2),
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("inline image"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Texture { bind_group, _texture: texture }
    }

    /// Draw one layer (0: under backgrounds, 1: under text, 2: over text).
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>, layer: usize) {
        let mut bound = false;
        for d in self.draws.iter().filter(|d| d.layer == layer) {
            let Some(t) = self.textures.get(&d.key) else { continue };
            if !bound {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.globals_group, &[]);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                bound = true;
            }
            pass.set_bind_group(1, &t.bind_group, &[]);
            pass.draw(0..4, d.range.clone());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.draws.is_empty()
    }
}
