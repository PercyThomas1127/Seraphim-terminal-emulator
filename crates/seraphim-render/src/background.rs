//! The animated background layer (spec §4.4, ADR 0006 "layers").
//!
//! One full-screen shader drawn before the text pass. Besides the idle
//! animation it has two *moods* that event bindings will drive later:
//!
//! - **privileged** ([`Background::set_privileged`]): the palette eases
//!   toward the alert colors and the edges breathe red, for as long as the
//!   session runs as root.
//! - **install** ([`Background::pulse_install`]): a one-shot wave of light
//!   that rises from the bottom, trailing sparks.
//!
//! All animation is computed from the times passed to `prepare`, so a frame
//! is reproducible (headless tests render exact moments).

use std::time::Duration;

use crate::theme::{to_f32, Rgb};

/// Which scene the background draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackgroundStyle {
    /// Plain theme background; no extra GPU work and no animation frames.
    #[default]
    None,
    /// Folding curtains of light over a faint starfield.
    Aurora,
    /// Slow domain-warped clouds and stars.
    Nebula,
    /// Drifting parallax stars and a band of dust.
    Starfield,
}

impl BackgroundStyle {
    fn shader_id(self) -> f32 {
        match self {
            BackgroundStyle::None => 0.0,
            BackgroundStyle::Aurora => 1.0,
            BackgroundStyle::Nebula => 2.0,
            BackgroundStyle::Starfield => 3.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundConfig {
    pub style: BackgroundStyle,
    /// Strength of every effect over the plain background, 0..1.
    pub intensity: f32,
    /// Animation speed multiplier; 0 draws a still frame and stops redraws.
    pub speed: f32,
    /// Frame rate cap while animating (halved when unfocused).
    pub fps: u32,
    /// Scene colors; `None` takes rose, blue and cyan from the theme.
    pub colors: Option<[Rgb; 3]>,
    /// Colors the scene shifts to in a privileged (root) session.
    pub privileged_colors: [Rgb; 2],
    /// Color of the install wave.
    pub install_color: Rgb,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        BackgroundConfig {
            style: BackgroundStyle::None,
            intensity: 0.6,
            speed: 1.0,
            fps: 30,
            colors: None,
            privileged_colors: [[0xff, 0x3b, 0x4e], [0xff, 0x9a, 0x3c]],
            install_color: [0xff, 0xd9, 0x8a],
        }
    }
}

/// How long the privileged palette takes to ease in or out.
pub const PRIVILEGED_FADE: f32 = 0.9;
/// Length of one install wave.
pub const INSTALL_WAVE: f32 = 2.6;

/// Layout must match `Bg` in `background.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Uniforms {
    frame: [f32; 4],
    params: [f32; 4],
    base: [f32; 4],
    accent0: [f32; 4],
    accent1: [f32; 4],
    accent2: [f32; 4],
    alert0: [f32; 4],
    alert1: [f32; 4],
    surge: [f32; 4],
}
// SAFETY: plain-old-data, repr(C), all f32.
unsafe impl bytemuck::Zeroable for Uniforms {}
unsafe impl bytemuck::Pod for Uniforms {}

pub struct Background {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    config: BackgroundConfig,
    /// Time of the latest `prepare`, in seconds.
    now: f32,
    last: Option<f32>,
    /// Integrated animation clock (speed changes don't make it jump).
    anim: f32,
    privileged: bool,
    /// (value when the change started, start time)
    privileged_change: (f32, f32),
    install_start: Option<f32>,
}

impl Background {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Background {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("seraphim background"),
            source: wgpu::ShaderSource::Wgsl(include_str!("background.wgsl").into()),
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("background uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("background"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("background"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("background"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("background"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_bg"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bg"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Background {
            pipeline,
            uniforms,
            bind_group,
            config: BackgroundConfig::default(),
            now: 0.0,
            last: None,
            anim: 0.0,
            privileged: false,
            privileged_change: (0.0, f32::NEG_INFINITY),
            install_start: None,
        }
    }

    pub fn config(&self) -> &BackgroundConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: BackgroundConfig) {
        self.config = config;
    }

    pub fn is_enabled(&self) -> bool {
        self.config.style != BackgroundStyle::None
    }

    /// Ease into (or out of) the privileged palette.
    pub fn set_privileged(&mut self, on: bool) {
        if on != self.privileged {
            self.privileged_change = (self.privileged_amount(self.now), self.now);
            self.privileged = on;
        }
    }

    pub fn privileged(&self) -> bool {
        self.privileged
    }

    /// Start an install wave at the current time.
    pub fn pulse_install(&mut self) {
        self.install_start = Some(self.now);
    }

    /// Whether the window should keep producing frames.
    pub fn needs_frames(&self) -> bool {
        self.is_enabled() && (self.config.speed > 0.0 || self.is_transitioning())
    }

    fn is_transitioning(&self) -> bool {
        let fading = self.now - self.privileged_change.1 < PRIVILEGED_FADE;
        let waving = self.install_start.is_some_and(|s| self.now - s < INSTALL_WAVE);
        // The privileged edge glow breathes, so it animates while active.
        fading || waving || self.privileged
    }

    /// Target time between frames.
    pub fn frame_interval(&self, focused: bool) -> Duration {
        let fps = self.config.fps.clamp(1, 240) as f32 / if focused { 1.0 } else { 2.0 };
        Duration::from_secs_f32(1.0 / fps)
    }

    fn privileged_amount(&self, t: f32) -> f32 {
        let (from, start) = self.privileged_change;
        let to = if self.privileged { 1.0 } else { 0.0 };
        let k = ((t - start) / PRIVILEGED_FADE).clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        from + (to - from) * k
    }

    /// Phase 0..1 of the current install wave, or `None`.
    pub fn install_phase(&self) -> Option<f32> {
        let s = self.install_start?;
        let p = (self.now - s) / INSTALL_WAVE;
        (0.0..1.0).contains(&p).then_some(p)
    }

    /// Advance the clocks to `time` (seconds) and upload this frame.
    pub(crate) fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        size: (u32, u32),
        theme_bg: Rgb,
        theme_accents: [Rgb; 3],
        time: f32,
    ) {
        let dt = self.last.map_or(0.0, |l| (time - l).clamp(0.0, 0.25));
        self.last = Some(time);
        self.now = time;
        let alert = self.privileged_amount(time);
        // Root sessions feel more restless.
        self.anim += dt * self.config.speed * (1.0 + 0.6 * alert);
        // Keep shader inputs small enough for f32 precision.
        self.anim %= 7200.0;
        let wall = time % 7200.0;

        let c = &self.config;
        let accents = c.colors.unwrap_or(theme_accents);
        let rgb = |c: Rgb| to_f32(c, 1.0);
        let u = Uniforms {
            frame: [size.0.max(1) as f32, size.1.max(1) as f32, wall, c.intensity.clamp(0.0, 1.0)],
            params: [c.style.shader_id(), self.anim, alert, self.install_phase().unwrap_or(-1.0)],
            base: rgb(theme_bg),
            accent0: rgb(accents[0]),
            accent1: rgb(accents[1]),
            accent2: rgb(accents[2]),
            alert0: rgb(c.privileged_colors[0]),
            alert1: rgb(c.privileged_colors[1]),
            surge: rgb(c.install_color),
        };
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
    }

    pub(crate) fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
