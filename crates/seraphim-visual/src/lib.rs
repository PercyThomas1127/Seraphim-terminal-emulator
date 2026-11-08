//! # seraphim-visual
//!
//! The **Visual Engine** (spec §5) and **Animation Engine** (spec §6).
//!
//! A scene is a stack of layers drawn *behind* the terminal text: solid
//! colors, gradients, images and WGSL shaders, each with an animated
//! opacity and a blend mode. Text is always composited on top by the
//! renderer, so it stays crisp regardless of what is underneath (spec §4.4).
//!
//! Safety rules from the spec:
//! * A shader that fails to compile is reported and skipped; it never
//!   crashes the terminal (§13 "visual crashes must not crash terminal
//!   sessions"). Shaders are compiled inside a wgpu validation error scope.
//! * Nothing animates unless something actually changes over time:
//!   [`VisualEngine::animating`] tells the window whether to keep drawing
//!   frames. The clock can be paused (unfocused window) or frozen (reduced
//!   motion).

pub mod animation;

use std::path::PathBuf;
use std::time::Instant;

pub use animation::{Animated, Easing, Repeat, Transition};

/// A shader that ships with Seraphim.
pub struct BuiltinShader {
    pub name: &'static str,
    pub source: &'static str,
    /// Parameters and their defaults.
    pub params: &'static [(&'static str, f32)],
    /// Named colors (`color(C_<name>)` in WGSL) and their defaults.
    pub colors: &'static [(&'static str, Rgba)],
}

/// Built-in shader layers.
pub const BUILTIN_SHADERS: &[BuiltinShader] = &[
    BuiltinShader {
        name: "aurora",
        source: include_str!("shaders/aurora.wgsl"),
        params: &[("speed", 0.3), ("intensity", 0.8), ("hue", 0.78), ("spread", 1.0)],
        colors: &[],
    },
    BuiltinShader {
        name: "starfield",
        source: include_str!("shaders/starfield.wgsl"),
        params: &[("density", 0.5), ("speed", 1.0), ("twinkle", 1.5), ("size", 1.0)],
        colors: &[],
    },
    BuiltinShader {
        name: "heavens",
        source: include_str!("shaders/heavens.wgsl"),
        params: &[
            ("style", 1.0),
            ("intensity", 0.6),
            ("speed", 1.0),
            ("alert", 0.0),
            ("install", 1.0),
        ],
        colors: &[
            ("base", rgb(0x14, 0x12, 0x1f)),
            ("accent0", rgb(0xf5, 0xa6, 0xd0)),
            ("accent1", rgb(0x89, 0xa8, 0xff)),
            ("accent2", rgb(0x7f, 0xe0, 0xe8)),
            ("alert0", rgb(0xff, 0x3b, 0x4e)),
            ("alert1", rgb(0xff, 0x9a, 0x3c)),
            ("surge", rgb(0xff, 0xd9, 0x8a)),
        ],
    },
];

type Params = &'static [(&'static str, f32)];
type Colors = &'static [(&'static str, Rgba)];

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

const PRELUDE: &str = include_str!("shaders/prelude.wgsl");
const MAX_PARAMS: usize = 16;
const MAX_STOPS: usize = 8;

/// Straight RGBA, 0..1, sRGB space.
pub type Rgba = [f32; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
}

impl Blend {
    pub fn parse(s: &str) -> Option<Blend> {
        Some(match s {
            "normal" => Blend::Normal,
            "add" => Blend::Add,
            "multiply" => Blend::Multiply,
            "screen" => Blend::Screen,
            _ => return None,
        })
    }

    /// Fixed-function blending on premultiplied colors.
    fn state(self) -> wgpu::BlendState {
        use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as O};
        let c = |src, dst| C { src_factor: src, dst_factor: dst, operation: O::Add };
        match self {
            Blend::Normal => wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            Blend::Add => wgpu::BlendState { color: c(F::One, F::One), alpha: c(F::Zero, F::One) },
            // src * dst + dst * (1 - a): darkens by the layer's color.
            Blend::Multiply => wgpu::BlendState {
                color: c(F::Dst, F::OneMinusSrcAlpha),
                alpha: c(F::Zero, F::One),
            },
            // 1 - (1 - src)(1 - dst): lightens.
            Blend::Screen => {
                wgpu::BlendState { color: c(F::One, F::OneMinusSrc), alpha: c(F::Zero, F::One) }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fit {
    #[default]
    Cover,
    Contain,
    Stretch,
    Tile,
}

impl Fit {
    pub fn parse(s: &str) -> Option<Fit> {
        Some(match s {
            "cover" => Fit::Cover,
            "contain" => Fit::Contain,
            "stretch" => Fit::Stretch,
            "tile" => Fit::Tile,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerKind {
    Solid(Rgba),
    /// Up to 8 colors, evenly spaced, along `angle` degrees (0 = left to
    /// right, 90 = top to bottom).
    Gradient {
        colors: Vec<Rgba>,
        angle: f32,
    },
    /// PNG image.
    Image {
        path: PathBuf,
        fit: Fit,
    },
    /// A built-in shader by name, or WGSL source implementing `shade`.
    Shader {
        name: String,
        source: Option<String>,
    },
}

/// A layer as declared in configuration (spec §5.4).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerSpec {
    pub id: String,
    pub kind: LayerKind,
    pub opacity: f32,
    pub blend: Blend,
    /// Shader parameters (name, value); built-ins have defaults.
    pub params: Vec<(String, f32)>,
    /// Shader colors (name, value), read with `color(C_<name>)`.
    pub colors: Vec<(String, Rgba)>,
}

impl LayerSpec {
    pub fn new(id: impl Into<String>, kind: LayerKind) -> Self {
        LayerSpec {
            id: id.into(),
            kind,
            opacity: 1.0,
            blend: Blend::Normal,
            params: Vec::new(),
            colors: Vec::new(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    resolution: [f32; 2],
    time: f32,
    opacity: f32,
    params: [f32; MAX_PARAMS],
    colors: [[f32; 4]; MAX_STOPS],
    stops: [f32; MAX_STOPS],
    info: [f32; 4],
    image: [f32; 4],
}
// SAFETY: plain f32 arrays, repr(C), no padding (all 4-byte fields, sizes
// multiples of 16 as required by the WGSL uniform layout).
unsafe impl bytemuck::Zeroable for Uniforms {}
unsafe impl bytemuck::Pod for Uniforms {}

struct GpuLayer {
    id: String,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    opacity: Animated,
    params: Vec<(String, Animated)>,
    /// The shader reads `u.time`, so it changes every frame.
    uses_time: bool,
    static_data: Uniforms,
    /// Being faded out after a scene change; removed when invisible.
    leaving: bool,
}

/// Engine clock: seconds since creation, minus time spent paused.
#[derive(Clone, Copy, Debug)]
struct Clock {
    base: Instant,
    offset: f64,
    paused_at: Option<f64>,
}

impl Clock {
    fn raw(&self) -> f64 {
        self.base.elapsed().as_secs_f64() - self.offset
    }
    fn now(&self) -> f64 {
        self.paused_at.unwrap_or_else(|| self.raw())
    }
    fn pause(&mut self, paused: bool) {
        match (paused, self.paused_at) {
            (true, None) => self.paused_at = Some(self.raw()),
            (false, Some(at)) => {
                self.offset += self.raw() - at;
                self.paused_at = None;
            }
            _ => {}
        }
    }
}

pub struct VisualEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    empty_texture: wgpu::TextureView,
    layers: Vec<GpuLayer>,
    clock: Clock,
    paused: bool,
    reduced_motion: bool,
    /// Overrides the clock (tests, screenshots).
    fixed_time: Option<f64>,
    /// Multiplies every layer's opacity (window background opacity).
    global_opacity: f32,
}

impl VisualEngine {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("visual layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("visual layer"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("visual layer"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let empty_texture = create_texture(device, queue, 1, 1, &[0, 0, 0, 0]);
        VisualEngine {
            device: device.clone(),
            queue: queue.clone(),
            format,
            layout,
            pipeline_layout,
            sampler,
            empty_texture,
            layers: Vec::new(),
            clock: Clock { base: Instant::now(), offset: 0.0, paused_at: None },
            paused: false,
            reduced_motion: false,
            fixed_time: None,
            global_opacity: 1.0,
        }
    }

    /// Current time on the engine clock, in seconds.
    pub fn time(&self) -> f64 {
        self.fixed_time.unwrap_or_else(|| self.clock.now())
    }

    /// Use a fixed time instead of the clock (tests and screenshots).
    pub fn set_fixed_time(&mut self, t: Option<f64>) {
        self.fixed_time = t;
    }

    /// Pause the clock (e.g. while the window is unfocused). Time does not
    /// jump when resumed.
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.clock.pause(paused || self.reduced_motion);
    }

    /// Reduced motion (accessibility, spec §4.4/§6.4): the clock stops and
    /// transitions become instant.
    pub fn set_reduced_motion(&mut self, on: bool) {
        self.reduced_motion = on;
        self.clock.pause(on || self.paused);
    }

    /// Scale every layer's opacity (used for window background opacity).
    pub fn set_global_opacity(&mut self, opacity: f32) {
        self.global_opacity = opacity.clamp(0.0, 1.0);
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn layer_ids(&self) -> Vec<&str> {
        self.layers.iter().filter(|l| !l.leaving).map(|l| l.id.as_str()).collect()
    }

    /// Replace the scene. The new layers fade in (and old ones fade out)
    /// with `transition`. Returns a message for each layer that could not
    /// be built; the others are still shown.
    pub fn set_layers(&mut self, specs: &[LayerSpec], transition: Transition) -> Vec<String> {
        let now = self.time();
        let transition = self.effective(transition);
        for l in &mut self.layers {
            l.leaving = true;
            l.opacity.set(0.0, transition, now);
        }
        let mut errors = Vec::new();
        for spec in specs {
            match self.build(spec) {
                Ok(mut layer) => {
                    layer.opacity = Animated::new(0.0);
                    layer.opacity.set(spec.opacity.clamp(0.0, 1.0) as f64, transition, now);
                    self.layers.push(layer);
                }
                Err(e) => errors.push(format!("layer {:?}: {e}", spec.id)),
            }
        }
        self.drop_finished(now);
        errors
    }

    /// Reduced motion makes transitions instant; so does a paused clock
    /// (unfocused or hidden window), or a change made while nobody is
    /// looking would sit frozen half-way until the window is focused.
    fn effective(&self, t: Transition) -> Transition {
        if self.reduced_motion || (self.paused && self.fixed_time.is_none()) {
            Transition::Instant
        } else {
            t
        }
    }

    /// Animate a layer's opacity. Returns false if there is no such layer.
    pub fn set_opacity(&mut self, id: &str, value: f32, transition: Transition) -> bool {
        let (now, t) = (self.time(), self.effective(transition));
        match self.layers.iter_mut().find(|l| l.id == id && !l.leaving) {
            Some(l) => {
                l.opacity.set(value.clamp(0.0, 1.0) as f64, t, now);
                true
            }
            None => false,
        }
    }

    /// Animate a shader parameter. Returns false if the layer or parameter
    /// does not exist.
    pub fn set_param(&mut self, id: &str, name: &str, value: f32, transition: Transition) -> bool {
        let (now, t) = (self.time(), self.effective(transition));
        let Some(l) = self.layers.iter_mut().find(|l| l.id == id && !l.leaving) else {
            return false;
        };
        match l.params.iter_mut().find(|(n, _)| n == name) {
            Some((_, a)) => {
                a.set(value as f64, t, now);
                true
            }
            None => false,
        }
    }

    /// Whether the scene changes over time right now; the window keeps
    /// drawing frames only while this is true.
    pub fn animating(&self) -> bool {
        if self.fixed_time.is_none() && (self.paused || self.reduced_motion) {
            // A paused clock: only explicit transitions could still move,
            // and they are frozen too.
            return false;
        }
        let now = self.time();
        self.layers.iter().any(|l| {
            l.uses_time || l.opacity.active(now) || l.params.iter().any(|(_, a)| a.active(now))
        })
    }

    fn drop_finished(&mut self, now: f64) {
        self.layers.retain(|l| !(l.leaving && !l.opacity.active(now) && l.opacity.target() == 0.0));
    }

    /// Update uniforms for a `width` x `height` target. Call once per frame
    /// before [`VisualEngine::render`].
    pub fn prepare(&mut self, width: u32, height: u32) {
        let now = self.time();
        self.drop_finished(now);
        let shader_time = now as f32;
        for l in &mut self.layers {
            let mut u = l.static_data;
            u.resolution = [width.max(1) as f32, height.max(1) as f32];
            u.time = shader_time;
            u.opacity = l.opacity.value(now) as f32 * self.global_opacity;
            for (i, (_, a)) in l.params.iter_mut().enumerate().take(MAX_PARAMS) {
                u.params[i] = a.value(now) as f32;
            }
            self.queue.write_buffer(&l.uniforms, 0, bytemuck::bytes_of(&u));
        }
    }

    /// Draw all layers into the current pass (before the text).
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        for l in &self.layers {
            pass.set_pipeline(&l.pipeline);
            pass.set_bind_group(0, &l.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn build(&self, spec: &LayerSpec) -> Result<GpuLayer, String> {
        let mut data: Uniforms = bytemuck::Zeroable::zeroed();
        let mut texture = None;
        let mut params: Vec<(String, f32)> = Vec::new();
        let mut colors: Vec<(String, Rgba)> = Vec::new();
        let body: String = match &spec.kind {
            LayerKind::Solid(c) => {
                data.colors[0] = *c;
                include_str!("shaders/solid.wgsl").into()
            }
            LayerKind::Gradient { colors, angle } => {
                if colors.is_empty() || colors.len() > MAX_STOPS {
                    return Err(format!("a gradient needs 1 to {MAX_STOPS} colors"));
                }
                for (i, c) in colors.iter().enumerate() {
                    data.colors[i] = *c;
                    data.stops[i] =
                        if colors.len() == 1 { 0.0 } else { i as f32 / (colors.len() - 1) as f32 };
                }
                data.info[0] = colors.len() as f32;
                data.info[1] = angle.to_radians();
                include_str!("shaders/gradient.wgsl").into()
            }
            LayerKind::Image { path, fit } => {
                let (w, h, rgba) = load_png(path)?;
                let max = self.device.limits().max_texture_dimension_2d;
                if w > max || h > max {
                    return Err(format!(
                        "{}: {w}x{h} exceeds the GPU limit of {max}",
                        path.display()
                    ));
                }
                texture = Some(create_texture(&self.device, &self.queue, w, h, &rgba));
                data.image = [w as f32, h as f32, 0.0, 0.0];
                data.info[2] = *fit as u32 as f32;
                include_str!("shaders/image.wgsl").into()
            }
            LayerKind::Shader { name, source } => {
                let (src, param_defaults, color_defaults): (&str, Params, Colors) = match source {
                    Some(src) => (src.as_str(), &[], &[]),
                    None => {
                        let b =
                            BUILTIN_SHADERS.iter().find(|b| b.name == name).ok_or_else(|| {
                                let names: Vec<&str> =
                                    BUILTIN_SHADERS.iter().map(|b| b.name).collect();
                                format!("unknown shader {name:?} (built-in: {})", names.join(", "))
                            })?;
                        (b.source, b.params, b.colors)
                    }
                };
                let custom = source.is_some();
                params = with_defaults(param_defaults, &spec.params, custom)
                    .map_err(|n| no_such(name, "parameter", n, param_defaults))?;
                colors = with_defaults(color_defaults, &spec.colors, custom)
                    .map_err(|n| no_such(name, "color", n, color_defaults))?;
                src.to_string()
            }
        };
        if params.len() > MAX_PARAMS {
            return Err(format!("at most {MAX_PARAMS} shader parameters"));
        }
        let mut consts = String::new();
        for (i, (n, _)) in params.iter().enumerate() {
            if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || n.is_empty() {
                return Err(format!("parameter name {n:?} must be letters, digits or _"));
            }
            consts.push_str(&format!("const P_{n}: u32 = {i}u;\n"));
        }
        if colors.len() > MAX_STOPS {
            return Err(format!("at most {MAX_STOPS} shader colors"));
        }
        for (i, (n, c)) in colors.iter().enumerate() {
            if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || n.is_empty() {
                return Err(format!("color name {n:?} must be letters, digits or _"));
            }
            consts.push_str(&format!("const C_{n}: u32 = {i}u;\n"));
            data.colors[i] = *c;
        }
        let source = format!("{PRELUDE}\n{consts}\n{body}");
        let uses_time = body.contains("u.time");

        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&spec.id),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&spec.id),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: Some(spec.blend.state()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(shader_error(&err.to_string()));
        }

        let uniforms = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&spec.id),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view = texture.as_ref().unwrap_or(&self.empty_texture);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&spec.id),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Ok(GpuLayer {
            id: spec.id.clone(),
            pipeline,
            bind_group,
            uniforms,
            opacity: Animated::new(spec.opacity as f64),
            params: params.into_iter().map(|(n, v)| (n, Animated::new(v as f64))).collect(),
            uses_time,
            static_data: data,
            leaving: false,
        })
    }
}

/// Defaults overridden by `values`. A custom shader takes `values` as they
/// are; a built-in rejects names it does not have (returned as the error).
fn with_defaults<T: Copy>(
    defaults: &[(&str, T)],
    values: &[(String, T)],
    custom: bool,
) -> Result<Vec<(String, T)>, String> {
    if custom {
        return Ok(values.to_vec());
    }
    let mut out: Vec<(String, T)> = defaults.iter().map(|(n, v)| (n.to_string(), *v)).collect();
    for (n, v) in values {
        match out.iter_mut().find(|(k, _)| k == n) {
            Some(slot) => slot.1 = *v,
            None => return Err(n.clone()),
        }
    }
    Ok(out)
}

fn no_such<T>(shader: &str, what: &str, name: String, defaults: &[(&str, T)]) -> String {
    let known: Vec<&str> = defaults.iter().map(|d| d.0).collect();
    let known = if known.is_empty() { "none".to_string() } else { known.join(", ") };
    format!("shader {shader:?} has no {what} {name:?} (it has: {known})")
}

/// Keep the useful part of a wgpu/naga validation error.
fn shader_error(raw: &str) -> String {
    let lines: Vec<&str> =
        raw.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    format!("shader did not compile:\n{}", lines.join("\n"))
}

fn create_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    rgba: &[u8],
) -> wgpu::TextureView {
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("visual image"),
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
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4 * w),
            rows_per_image: Some(h),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Decode a PNG to RGBA8.
fn load_png(path: &std::path::Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("image too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let px = (info.width * info.height) as usize;
    let data = &buf[..info.buffer_size()];
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    };
    if rgba.len() != px * 4 {
        return Err(format!("{}: unexpected PNG layout", path.display()));
    }
    Ok((info.width, info.height, rgba))
}
