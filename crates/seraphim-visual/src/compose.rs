//! Offscreen compositing, used only when a scene needs it: backdrop blur
//! layers (`kind = "blur"`) and styled scene transitions (spec 6.3: slide,
//! wipe, dissolve, zoom, blur, user WGSL). Otherwise layers draw straight
//! into the frame.
//!
//! Scenes render into premultiplied RGBA targets cleared to transparent.
//! A blur layer blurs what is under it: target -> (horizontal) scratch ->
//! (vertical) scratch -> blended back with the layer's opacity, at reduced
//! resolution for large radii. A transition renders the old and the new
//! scene and mixes them with a transition shader:
//!
//! ```wgsl
//! fn transition(uv: vec2<f32>, p: f32) -> vec4<f32>
//! ```
//!
//! returning premultiplied RGBA, with `from_scene(uv)` / `to_scene(uv)`,
//! `u.resolution` and `u.time` available.

use std::collections::HashMap;

const POST_PRELUDE: &str = include_str!("shaders/post.wgsl");

/// Built-in transitions.
pub const BUILTIN_TRANSITIONS: &[(&str, &str)] = &[
    ("slide", include_str!("shaders/transitions/slide.wgsl")),
    ("wipe", include_str!("shaders/transitions/wipe.wgsl")),
    ("dissolve", include_str!("shaders/transitions/dissolve.wgsl")),
    ("zoom", include_str!("shaders/transitions/zoom.wgsl")),
    ("blur", include_str!("shaders/transitions/blur.wgsl")),
    ("fade-through", include_str!("shaders/transitions/fade_through.wgsl")),
];

/// How scenes change.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TransitionStyle {
    /// Layer opacities crossfade (no offscreen work).
    #[default]
    Fade,
    /// A built-in transition by name.
    Builtin(String),
    /// WGSL implementing `transition` (a user file).
    Custom { name: String, source: String },
}

impl TransitionStyle {
    /// `"fade"`, a built-in name, or WGSL source from a file.
    pub fn named(name: &str) -> Option<TransitionStyle> {
        match name {
            "fade" | "" => Some(TransitionStyle::Fade),
            n if BUILTIN_TRANSITIONS.iter().any(|(b, _)| *b == n) => {
                Some(TransitionStyle::Builtin(n.to_owned()))
            }
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Post {
    resolution: [f32; 2],
    time: f32,
    progress: f32,
    dir: [f32; 2],
    radius: f32,
    opacity: f32,
}
// SAFETY: plain f32s, repr(C), 32 bytes.
unsafe impl bytemuck::Zeroable for Post {}
unsafe impl bytemuck::Pod for Post {}

const SLOT: u64 = 256; // >= min_uniform_buffer_offset_alignment everywhere
const SLOTS: u64 = 64;

fn begin_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    clear: bool,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("blur"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    })
}

/// A fullscreen post-process pipeline (prelude + `body` with `fs_post`),
/// compiled inside a validation scope so user shaders can't crash us.
fn build_post(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    layout: &wgpu::PipelineLayout,
    label: &str,
    body: &str,
    blend: Option<wgpu::BlendState>,
) -> Result<wgpu::RenderPipeline, String> {
    let source = format!("{POST_PRELUDE}\n{body}");
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_post"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_post"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    match pollster::block_on(scope.pop()) {
        Some(e) => Err(crate::shader_error(&e.to_string())),
        None => Ok(pipeline),
    }
}

pub(crate) struct Target {
    pub _texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: (u32, u32),
}

pub(crate) struct Compositor {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    next_slot: std::cell::Cell<u64>,
    blur: wgpu::RenderPipeline,
    copy: wgpu::RenderPipeline,
    transitions: HashMap<String, wgpu::RenderPipeline>,
    pub a: Option<Target>,
    pub b: Option<Target>,
    scratch: HashMap<(u32, u32), (Target, Target)>,
    empty: wgpu::TextureView,
}

fn target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    (w, h): (u32, u32),
    label: &str,
) -> Target {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Target { _texture: texture, view, size: (w.max(1), h.max(1)) }
}

impl Compositor {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        empty: wgpu::TextureView,
    ) -> Self {
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compose"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Post>() as u64),
                    },
                    count: None,
                },
                tex(1),
                tex(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compose"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("compose"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("compose uniforms"),
            size: SLOT * SLOTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blur = build_post(
            device,
            format,
            &pipeline_layout,
            "blur",
            include_str!("shaders/post_blur.wgsl"),
            None,
        )
        .expect("built-in blur");
        let copy = build_post(
            device,
            format,
            &pipeline_layout,
            "copy",
            include_str!("shaders/post_copy.wgsl"),
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        )
        .expect("built-in copy");
        Compositor {
            format,
            layout,
            pipeline_layout,
            sampler,
            uniforms,
            next_slot: std::cell::Cell::new(0),
            blur,
            copy,
            transitions: HashMap::new(),
            a: None,
            b: None,
            scratch: HashMap::new(),
            empty,
        }
    }

    fn pipeline(
        &self,
        device: &wgpu::Device,
        label: &str,
        body: &str,
        blend: Option<wgpu::BlendState>,
    ) -> Result<wgpu::RenderPipeline, String> {
        build_post(device, self.format, &self.pipeline_layout, label, body, blend)
    }

    /// Compile (once) the pipeline for a transition style.
    pub fn prepare_transition(
        &mut self,
        device: &wgpu::Device,
        style: &TransitionStyle,
    ) -> Result<(), String> {
        let (key, body) = match style {
            TransitionStyle::Fade => return Ok(()),
            TransitionStyle::Builtin(n) => {
                let body = BUILTIN_TRANSITIONS
                    .iter()
                    .find(|(b, _)| b == n)
                    .map(|t| t.1)
                    .ok_or("unknown transition")?;
                (n.clone(), body.to_string())
            }
            TransitionStyle::Custom { name, source } => {
                (format!("custom:{name}:{}", source.len()), source.clone())
            }
        };
        if self.transitions.contains_key(&key) {
            return Ok(());
        }
        let wrapped = format!("{body}\n@fragment fn fs_post(in: PostOut) -> @location(0) vec4<f32> {{ return transition(in.uv, u.progress); }}\n");
        let p = self.pipeline(
            device,
            &key,
            &wrapped,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        )?;
        self.transitions.insert(key, p);
        Ok(())
    }

    fn transition_key(style: &TransitionStyle) -> Option<String> {
        match style {
            TransitionStyle::Fade => None,
            TransitionStyle::Builtin(n) => Some(n.clone()),
            TransitionStyle::Custom { name, source } => {
                Some(format!("custom:{name}:{}", source.len()))
            }
        }
    }

    /// Make sure the scene targets match the frame size.
    pub fn ensure_targets(&mut self, device: &wgpu::Device, size: (u32, u32), two: bool) {
        let fits = |t: &Option<Target>| t.as_ref().is_some_and(|t| t.size == size);
        if !fits(&self.b) {
            self.b = Some(target(device, self.format, size, "scene"));
        }
        if two && !fits(&self.a) {
            self.a = Some(target(device, self.format, size, "outgoing scene"));
        }
    }

    pub fn begin_frame(&mut self) {
        self.next_slot.set(0);
    }

    fn slot(&self, queue: &wgpu::Queue, post: Post) -> u32 {
        let slot = self.next_slot.get().min(SLOTS - 1);
        self.next_slot.set(self.next_slot.get() + 1);
        queue.write_buffer(&self.uniforms, slot * SLOT, bytemuck::bytes_of(&post));
        (slot * SLOT) as u32
    }

    fn bind(
        &self,
        device: &wgpu::Device,
        a: &wgpu::TextureView,
        b: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniforms,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<Post>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(a),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(b),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Blur `scene` (target A or B) in place by `radius` pixels, blending
    /// the result back with `opacity`.
    pub fn blur(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        use_a: bool,
        radius: f32,
        opacity: f32,
    ) {
        if radius <= 0.0 || opacity <= 0.0 {
            return;
        }
        let size =
            if use_a { self.a.as_ref() } else { self.b.as_ref() }.map(|t| t.size).unwrap_or((1, 1));
        // Large radii blur at lower resolution (same look, far fewer taps).
        let scale = (radius / 6.0).clamp(1.0, 8.0);
        let small =
            (((size.0 as f32 / scale) as u32).max(1), ((size.1 as f32 / scale) as u32).max(1));
        if !self.scratch.contains_key(&small) {
            if self.scratch.len() > 4 {
                self.scratch.clear();
            }
            let pair = (
                target(device, self.format, small, "blur 1"),
                target(device, self.format, small, "blur 2"),
            );
            self.scratch.insert(small, pair);
        }
        let r = radius / scale;
        let res = [small.0 as f32, small.1 as f32];
        let h = self.slot(
            queue,
            Post {
                resolution: res,
                dir: [1.0, 0.0],
                radius: r,
                opacity: 1.0,
                ..Default::default()
            },
        );
        let v = self.slot(
            queue,
            Post {
                resolution: res,
                dir: [0.0, 1.0],
                radius: r,
                opacity: 1.0,
                ..Default::default()
            },
        );
        let back = self.slot(
            queue,
            Post { resolution: [size.0 as f32, size.1 as f32], opacity, ..Default::default() },
        );
        let (s1, s2) = &self.scratch[&small];
        let scene = if use_a { self.a.as_ref() } else { self.b.as_ref() }.expect("targets exist");
        let pass = begin_pass;
        let g1 = self.bind(device, &scene.view, &self.empty);
        let g2 = self.bind(device, &s1.view, &self.empty);
        let g3 = self.bind(device, &s2.view, &self.empty);
        {
            let mut p = pass(encoder, &s1.view, true);
            p.set_pipeline(&self.blur);
            p.set_bind_group(0, &g1, &[h]);
            p.draw(0..3, 0..1);
        }
        {
            let mut p = pass(encoder, &s2.view, true);
            p.set_pipeline(&self.blur);
            p.set_bind_group(0, &g2, &[v]);
            p.draw(0..3, 0..1);
        }
        {
            let mut p = pass(encoder, &scene.view, false);
            p.set_pipeline(&self.copy);
            p.set_bind_group(0, &g3, &[back]);
            p.draw(0..3, 0..1);
        }
    }

    /// Draw the composed scene into the frame: target B alone, or A -> B
    /// through the transition at `progress`.
    pub fn present(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'_>,
        style: Option<&TransitionStyle>,
        progress: f32,
        time: f32,
    ) {
        let Some(b) = self.b.as_ref() else { return };
        let res = [b.size.0 as f32, b.size.1 as f32];
        let pipeline = style.and_then(Self::transition_key).and_then(|k| self.transitions.get(&k));
        match (pipeline, self.a.as_ref()) {
            (Some(p), Some(a)) => {
                let off = self.slot(
                    queue,
                    Post { resolution: res, time, progress, opacity: 1.0, ..Default::default() },
                );
                let g = self.bind(device, &a.view, &b.view);
                pass.set_pipeline(p);
                pass.set_bind_group(0, &g, &[off]);
                pass.draw(0..3, 0..1);
            }
            _ => {
                let off =
                    self.slot(queue, Post { resolution: res, opacity: 1.0, ..Default::default() });
                let g = self.bind(device, &b.view, &self.empty);
                pass.set_pipeline(&self.copy);
                pass.set_bind_group(0, &g, &[off]);
                pass.draw(0..3, 0..1);
            }
        }
    }
}
