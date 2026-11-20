//! # seraphim-render
//!
//! The Seraphim **Rendering Engine** text path (spec §4.1, §4.3): turns a
//! read-only [`Terminal`] into GPU draw data.
//!
//! Everything on screen is one instanced quad (background run, glyph,
//! decoration or cursor piece), drawn in a single call from one pipeline
//! with one glyph atlas. The renderer is independent of windowing: give it
//! a `wgpu::Device`, a target format and a render pass. [`headless`]
//! renders to an image for tests and screenshots.
//!
//! The Visual Engine (Phase II) will draw its layers *before* this pass;
//! text is always composited on top so it stays crisp (spec §4.4).

mod atlas;
pub mod cursorfx;
pub mod font;
pub mod gpu;
pub mod headless;
mod images;
mod theme;

use std::collections::HashMap;

use seraphim_terminal::{Attrs, CursorShape, Flags, Point, SelectionRange, Terminal};

use atlas::{Atlas, AtlasRect, ATLAS_SIZE, COLOR_ATLAS_SIZE};
use font::{is_emoji_cluster, is_ignorable, is_private_use, GlyphKey, ShapedGlyph};
pub use font::{
    load_system_fonts_in_background, rescan_system_fonts, CellMetrics, FontConfig, FontError,
    FontStyle, Fonts, BUILTIN_FAMILY,
};
use theme::to_f32;
pub use theme::{Rgb, Theme};
use unicode_normalization::UnicodeNormalization;

/// One quad. Layout must match `Instance` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    kind: u32,
    /// Effect parameters (glow, ring); zero otherwise.
    params: [f32; 3],
}

// SAFETY: plain-old-data, repr(C), no padding beyond the explicit field.
unsafe impl bytemuck::Zeroable for Instance {}
unsafe impl bytemuck::Pod for Instance {}

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;
/// Soft glow around an inner rectangle: params = (radius, quad w, quad h).
const KIND_GLOW: u32 = 3;
/// Ring: params = (radius, thickness, quad size).
const KIND_RING: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct Globals {
    screen: [f32; 2],
    atlas: [f32; 2],
    color_atlas: [f32; 2],
    _pad: [f32; 2],
}
unsafe impl bytemuck::Zeroable for Globals {}
unsafe impl bytemuck::Pod for Globals {}

/// Per-frame view parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub width: u32,
    pub height: u32,
    /// Inner padding around the grid, in pixels.
    pub padding: f32,
    /// Unfocused windows draw a hollow cursor.
    pub focused: bool,
    /// Whether the cursor blink phase is currently "on".
    pub cursor_on: bool,
}

#[derive(Clone, Copy)]
struct CachedGlyph {
    /// Where the pixels are (in the mask atlas, or the color atlas).
    rect: AtlasRect,
    /// Size of the glyph as rasterized, which placement uses. Larger than
    /// `rect` for color bitmaps shrunk before upload.
    w: u32,
    h: u32,
    left: i32,
    top: i32,
    color: bool,
}

/// Statistics about the last prepared frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub instances: usize,
    pub glyphs_rasterized: usize,
    pub atlas_resets: usize,
    /// Rows whose instances were built this frame (the rest were reused).
    pub rows_rebuilt: usize,
    /// Instance bytes sent to the GPU this frame.
    pub uploaded_bytes: usize,
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    /// Coverage masks (one byte per texel).
    mask_texture: wgpu::Texture,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    atlas: Atlas,
    /// Color glyphs (emoji), created when the first one is drawn; until
    /// then a 1x1 placeholder is bound in its place.
    color_texture: Option<wgpu::Texture>,
    color_atlas: Atlas,
    device: wgpu::Device,
    glyphs: HashMap<GlyphKey, Option<CachedGlyph>>,
    /// Shaping results per (face, cluster text).
    shaped: HashMap<(u16, String), Vec<ShapedGlyph>>,
    fonts: Fonts,
    /// The configured theme.
    base_theme: Theme,
    /// `base_theme` plus the terminal's color overrides, for this frame.
    theme: Theme,
    applied_overrides: seraphim_terminal::palette::ColorOverrides,
    instances: Vec<Instance>,
    stats: FrameStats,
    /// Instances before this index are cell backgrounds (and the block
    /// cursor); images below text are drawn between the two halves.
    bg_end: u32,
    images: images::Images,
    fx: cursorfx::Fx,
    /// Instances of each visible row from earlier frames (partial redraw,
    /// spec §4.4): a row is rebuilt only when it, its selection, or the
    /// cursor on it changed.
    rows_cache: Vec<RowCache>,
    /// Cell size, padding and columns the cache was built for.
    cache_key: Option<[u32; 5]>,
    /// The instances the GPU buffer holds.
    uploaded: Vec<Instance>,
    /// What the selected slot's instances were last prepared from; see
    /// [`Renderer::is_prepared`].
    stamp: Option<Stamp>,
    /// The selected slot draws without cursor and selection effects (see
    /// [`Renderer::select_plain`]).
    plain: bool,
    /// What new slots need.
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    mask_view: wgpu::TextureView,
    color_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    linear_sampler: wgpu::Sampler,
    /// Bumped whenever prepared instances may no longer match what a fresh
    /// prepare would build: atlas resets, theme, font and effect changes.
    epoch: u64,
    /// The slot whose state is in the fields above, and the others.
    current: u64,
    slots: HashMap<u64, Slot>,
}

/// One pane's draw state. The renderer holds the selected slot's state in
/// its own fields and parks the others here; the glyph atlas, fonts,
/// shaping cache and pipeline are shared.
struct Slot {
    globals: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    bg_end: u32,
    images: images::Images,
    fx: cursorfx::Fx,
    theme: Theme,
    applied_overrides: seraphim_terminal::palette::ColorOverrides,
    rows_cache: Vec<RowCache>,
    cache_key: Option<[u32; 5]>,
    uploaded: Vec<Instance>,
    stamp: Option<Stamp>,
    plain: bool,
}

/// A caller's content version, the view, and the renderer epoch that a
/// slot's instances were prepared with.
type Stamp = (u64, View, u64);

/// What a row's instances depend on besides the global key.
#[derive(Clone, Debug, PartialEq)]
struct RowKey {
    row: seraphim_terminal::Row,
    /// Selected columns (empty: none), and the selection's fade.
    selected: Vec<bool>,
    sel_alpha: u32,
    /// Column of a block cursor inverting the text in this row.
    inverted: Option<usize>,
}

#[derive(Default)]
struct RowCache {
    key: Option<RowKey>,
    bg: Vec<Instance>,
    fg: Vec<Instance>,
}

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        fonts: Fonts,
        theme: Theme,
    ) -> Renderer {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("seraphim cells"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let mask_texture =
            atlas_texture(device, "glyph atlas", ATLAS_SIZE, wgpu::TextureFormat::R8Unorm);
        let mask_view = mask_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_view =
            atlas_texture(device, "no color glyphs", 1, wgpu::TextureFormat::Rgba8Unorm)
                .create_view(&wgpu::TextureViewDescriptor::default());
        // Glyphs are placed on whole pixels at 1:1 scale, so nearest
        // sampling is exact and never blurs text.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let linear_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("emoji sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cells"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Color glyphs, read through the linear sampler.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = cells_bind_group(
            device,
            &layout,
            &globals,
            [&mask_view, &color_view],
            [&sampler, &linear_sampler],
        );
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cells"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32, 4 => Float32x3];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cells"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
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
        let instance_capacity = 4096;
        let instance_buffer = Self::make_instance_buffer(device, instance_capacity);
        let images = images::Images::new(device, format, &globals);
        Renderer {
            pipeline,
            bind_group,
            globals,
            mask_texture,
            instance_buffer,
            instance_capacity,
            instance_count: 0,
            atlas: Atlas::new(ATLAS_SIZE),
            color_texture: None,
            color_atlas: Atlas::new(COLOR_ATLAS_SIZE),
            device: device.clone(),
            glyphs: HashMap::new(),
            shaped: HashMap::new(),
            fonts,
            base_theme: theme.clone(),
            theme,
            applied_overrides: Default::default(),
            instances: Vec::new(),
            stats: FrameStats::default(),
            bg_end: 0,
            images,
            fx: Default::default(),
            rows_cache: Vec::new(),
            cache_key: None,
            uploaded: Vec::new(),
            stamp: None,
            plain: false,
            format,
            layout,
            mask_view,
            color_view,
            sampler,
            linear_sampler,
            epoch: 0,
            current: 0,
            slots: HashMap::new(),
        }
    }

    fn new_slot(&self, device: &wgpu::Device) -> Slot {
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self.bind_group_for(device, &globals);
        let images = images::Images::new(device, self.format, &globals);
        let mut fx = cursorfx::Fx::default();
        fx.cfg = self.fx.cfg;
        fx.clock = self.fx.clock;
        Slot {
            globals,
            bind_group,
            instance_buffer: Self::make_instance_buffer(device, 1024),
            instance_capacity: 1024,
            instance_count: 0,
            bg_end: 0,
            images,
            fx,
            theme: self.base_theme.clone(),
            applied_overrides: Default::default(),
            rows_cache: Vec::new(),
            cache_key: None,
            uploaded: Vec::new(),
            stamp: None,
            plain: false,
        }
    }

    fn bind_group_for(&self, device: &wgpu::Device, globals: &wgpu::Buffer) -> wgpu::BindGroup {
        cells_bind_group(
            device,
            &self.layout,
            globals,
            [&self.mask_view, &self.color_view],
            [&self.sampler, &self.linear_sampler],
        )
    }

    /// Create the color glyph atlas and bind it in every slot.
    fn create_color_atlas(&mut self) {
        let texture = atlas_texture(
            &self.device,
            "color glyph atlas",
            COLOR_ATLAS_SIZE,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        self.color_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.color_texture = Some(texture);
        let device = self.device.clone();
        self.bind_group = self.bind_group_for(&device, &self.globals);
        let groups: Vec<(u64, wgpu::BindGroup)> = self
            .slots
            .iter()
            .map(|(&id, s)| (id, self.bind_group_for(&device, &s.globals)))
            .collect();
        for (id, g) in groups {
            if let Some(s) = self.slots.get_mut(&id) {
                s.bind_group = g;
            }
        }
    }

    /// Forget every slot's cached rows (glyph positions or colors moved).
    fn clear_row_caches(&mut self) {
        self.epoch += 1;
        self.rows_cache.clear();
        for s in self.slots.values_mut() {
            s.rows_cache.clear();
        }
    }

    /// Swap the selected slot's state with `slot`'s.
    fn swap_slot(&mut self, slot: &mut Slot) {
        std::mem::swap(&mut self.globals, &mut slot.globals);
        std::mem::swap(&mut self.bind_group, &mut slot.bind_group);
        std::mem::swap(&mut self.instance_buffer, &mut slot.instance_buffer);
        std::mem::swap(&mut self.instance_capacity, &mut slot.instance_capacity);
        std::mem::swap(&mut self.instance_count, &mut slot.instance_count);
        std::mem::swap(&mut self.bg_end, &mut slot.bg_end);
        std::mem::swap(&mut self.images, &mut slot.images);
        std::mem::swap(&mut self.fx, &mut slot.fx);
        std::mem::swap(&mut self.theme, &mut slot.theme);
        std::mem::swap(&mut self.applied_overrides, &mut slot.applied_overrides);
        std::mem::swap(&mut self.rows_cache, &mut slot.rows_cache);
        std::mem::swap(&mut self.cache_key, &mut slot.cache_key);
        std::mem::swap(&mut self.uploaded, &mut slot.uploaded);
        std::mem::swap(&mut self.stamp, &mut slot.stamp);
        std::mem::swap(&mut self.plain, &mut slot.plain);
    }

    /// Draw state for pane `id` from now on (created on first use). A
    /// window with several panes calls this before each pane's
    /// [`Renderer::prepare`] and [`Renderer::render`]; panes share the
    /// glyph atlas and fonts. Slot 0 exists from the start.
    pub fn select(&mut self, device: &wgpu::Device, id: u64) {
        self.select_slot(device, id, false);
    }

    /// Like [`Renderer::select`], for a slot that never draws cursor or
    /// selection effects, whatever [`Renderer::set_cursor_effects`] sets
    /// (menus, bars and other interface text).
    pub fn select_plain(&mut self, device: &wgpu::Device, id: u64) {
        self.select_slot(device, id, true);
    }

    fn select_slot(&mut self, device: &wgpu::Device, id: u64, plain: bool) {
        if id == self.current {
            return;
        }
        let mut target = match self.slots.remove(&id) {
            Some(s) => s,
            None => {
                let mut s = self.new_slot(device);
                if plain {
                    s.fx.cfg = Default::default();
                    s.plain = true;
                }
                s
            }
        };
        self.swap_slot(&mut target);
        // `target` now holds the previously selected slot's state.
        self.slots.insert(self.current, target);
        self.current = id;
    }

    /// Forget a pane's draw state.
    pub fn drop_slot(&mut self, id: u64) {
        self.slots.remove(&id);
    }

    /// The selected slot.
    pub fn slot(&self) -> u64 {
        self.current
    }

    fn make_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Cursor and selection effects (none by default).
    pub fn set_cursor_effects(&mut self, fx: cursorfx::CursorEffects) {
        self.epoch += 1;
        if !self.plain {
            self.fx.cfg = fx;
        }
        for s in self.slots.values_mut().filter(|s| !s.plain) {
            s.fx.cfg = fx;
        }
    }

    /// Use a fixed time for effects instead of the clock (tests).
    pub fn set_effects_clock(&mut self, now: Option<std::time::Instant>) {
        self.fx.clock = now;
        for s in self.slots.values_mut() {
            s.fx.clock = now;
        }
    }

    /// Whether the selected slot already holds what [`Renderer::prepare`]
    /// would build for content `version` (e.g. the session's render
    /// generation) at `view`: nothing it depends on changed since
    /// [`Renderer::mark_prepared`] was called with the same arguments, and
    /// no effect or image animation is running. The caller can then skip
    /// `prepare` for this frame and draw the slot as it is.
    pub fn is_prepared(&self, version: u64, view: &View) -> bool {
        !self.fx.animating()
            && !self.images.animating
            && self.stamp == Some((version, *view, self.epoch))
    }

    /// Record that the selected slot was just prepared from content
    /// `version` at `view` (see [`Renderer::is_prepared`]).
    pub fn mark_prepared(&mut self, version: u64, view: View) {
        self.stamp = Some((version, view, self.epoch));
    }

    /// Changes whenever slots prepared earlier may be stale (atlas reset,
    /// theme or font change). A caller preparing several slots in one frame
    /// compares it before and after, and prepares them again if it moved.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// A key was pressed (for typing ripples).
    pub fn key_pressed(&mut self) {
        self.fx.ripple();
    }

    /// An effect is in motion: the window should keep drawing frames.
    pub fn effects_animating(&self) -> bool {
        self.fx.animating()
            || self.images.animating
            || self.slots.values().any(|s| s.fx.animating() || s.images.animating)
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        self.fonts.metrics()
    }

    /// The configured theme (without application color overrides).
    #[allow(clippy::misnamed_getters)] // intentionally not the per-frame theme
    pub fn theme(&self) -> &Theme {
        &self.base_theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.base_theme = theme.clone();
        for s in self.slots.values_mut() {
            s.theme = theme.clone();
            s.applied_overrides = Default::default();
        }
        self.theme = theme;
        self.applied_overrides = Default::default();
        self.clear_row_caches();
    }

    /// Replace fonts (e.g. on scale-factor or font-size change).
    pub fn set_fonts(&mut self, fonts: Fonts) {
        self.fonts = fonts;
        self.glyphs.clear();
        self.shaped.clear();
        self.atlas.clear();
        self.color_atlas.clear();
        self.clear_row_caches();
    }

    /// Call when the background system font scan has finished (see
    /// [`load_system_fonts_in_background`]). Returns whether characters were
    /// left blank while it ran, in which case the next frame redraws every
    /// row with the fallback fonts now available.
    pub fn system_fonts_ready(&mut self) -> bool {
        if !self.fonts.take_early_misses() {
            return false;
        }
        self.shaped.clear();
        self.clear_row_caches();
        true
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// Grid dimensions that fit in a `width` x `height` pixel surface.
    pub fn grid_size(&self, width: u32, height: u32, padding: f32) -> (u16, u16) {
        let m = self.fonts.metrics();
        let cols = ((width as f32 - 2.0 * padding) / m.width).floor().max(1.0);
        let rows = ((height as f32 - 2.0 * padding) / m.height).floor().max(1.0);
        (cols.min(u16::MAX as f32) as u16, rows.min(u16::MAX as f32) as u16)
    }

    /// Color the surface should be cleared to before [`Renderer::render`].
    pub fn clear_color(&self) -> wgpu::Color {
        let c = to_f32(self.theme.background, 1.0);
        wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: 1.0 }
    }

    /// Build and upload this frame's draw data.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        term: &Terminal,
        view: View,
    ) {
        self.stamp = None;
        self.stats = FrameStats::default();
        self.build_instances(queue, term, &view);
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = Self::make_instance_buffer(device, self.instance_capacity);
            self.uploaded.clear();
        }
        // Upload only the span that differs from what the GPU has.
        let new: &[u8] = bytemuck::cast_slice(&self.instances);
        let old: &[u8] = bytemuck::cast_slice(&self.uploaded);
        let size = std::mem::size_of::<Instance>();
        let (first, last) = if new == old {
            (0, 0)
        } else {
            // Compare whole instances (memcmp), not bytes.
            let pairs = || new.chunks_exact(size).zip(old.chunks_exact(size));
            let first =
                pairs().position(|(a, b)| a != b).unwrap_or(new.len().min(old.len()) / size);
            let last = if new.len() == old.len() {
                pairs().rposition(|(a, b)| a != b).map_or(first, |i| i + 1)
            } else {
                new.len() / size
            };
            (first * size, last * size)
        };
        if last > first {
            queue.write_buffer(&self.instance_buffer, first as u64, &new[first..last]);
        }
        self.stats.uploaded_bytes = last.saturating_sub(first);
        self.uploaded.clear();
        self.uploaded.extend_from_slice(&self.instances);
        let globals = Globals {
            screen: [view.width.max(1) as f32, view.height.max(1) as f32],
            atlas: [ATLAS_SIZE as f32, ATLAS_SIZE as f32],
            color_atlas: [COLOR_ATLAS_SIZE as f32, COLOR_ATLAS_SIZE as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        let m = self.fonts.metrics();
        let now = self.fx.now();
        self.images.prepare(device, queue, term, view.padding.round(), (m.width, m.height), now);
        self.instance_count = self.instances.len() as u32;
        self.stats.instances = self.instances.len();
    }

    /// Fill the selected slot with plain rectangles (`[x, y, w, h]` in
    /// pixels of a `width` x `height` target, premultiplied RGBA): pane
    /// dividers, focus accents. Draw it with [`Renderer::render`].
    pub fn prepare_rects(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        rects: &[([f32; 4], [f32; 4])],
    ) {
        self.stamp = None;
        self.instances.clear();
        for ([x, y, w, h], c) in rects {
            self.push_rect(*x, *y, *w, *h, *c);
        }
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = Self::make_instance_buffer(device, self.instance_capacity);
        }
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&self.instances));
        self.uploaded.clear();
        self.uploaded.extend_from_slice(&self.instances);
        let globals = Globals {
            screen: [width.max(1) as f32, height.max(1) as f32],
            atlas: [ATLAS_SIZE as f32, ATLAS_SIZE as f32],
            color_atlas: [COLOR_ATLAS_SIZE as f32, COLOR_ATLAS_SIZE as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        self.instance_count = self.instances.len() as u32;
        self.bg_end = self.instance_count;
    }

    /// Record the draw into an existing render pass.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.images.is_empty() {
            if self.instance_count > 0 {
                self.draw_cells(pass, 0..self.instance_count);
            }
            return;
        }
        // Kitty z-layers: under backgrounds, under text, over text.
        let split = self.bg_end.min(self.instance_count);
        self.images.render(pass, 0);
        self.draw_cells(pass, 0..split);
        self.images.render(pass, 1);
        self.draw_cells(pass, split..self.instance_count);
        self.images.render(pass, 2);
    }

    fn draw_cells(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..4, range);
    }

    fn build_instances(&mut self, queue: &wgpu::Queue, term: &Terminal, view: &View) {
        // If the atlas fills mid-frame, clear it and rebuild once.
        for attempt in 0..2 {
            if self.try_build(queue, term, view) {
                return;
            }
            self.glyphs.clear();
            self.atlas.clear();
            self.color_atlas.clear();
            // Cached rows point into the old atlas (in every pane).
            self.clear_row_caches();
            self.stats.atlas_resets += 1;
            if attempt == 1 {
                // Pathological screen (more distinct glyphs than fit); draw
                // what we could.
                return;
            }
        }
    }

    /// Returns false if the atlas overflowed.
    fn try_build(&mut self, queue: &wgpu::Queue, term: &Terminal, view: &View) -> bool {
        self.instances.clear();
        if *term.color_overrides() != self.applied_overrides {
            self.applied_overrides = term.color_overrides().clone();
            self.theme = self.base_theme.with_overrides(&self.applied_overrides);
            self.rows_cache.clear();
        }
        let m = self.fonts.metrics();
        let (cw, ch) = (m.width, m.height);
        let pad = view.padding.round();
        let (rows, cols) = (term.rows(), term.cols());
        // Cached rows hold extras indexes: renumbering them invalidates.
        let key = [
            cw.to_bits(),
            ch.to_bits(),
            pad.to_bits(),
            cols as u32,
            term.extras_generation() as u32,
        ];
        if self.cache_key != Some(key) {
            self.rows_cache.clear();
            self.cache_key = Some(key);
        }
        let offset = term.display_offset();
        let selection = term.selection_range();
        let cursor = term.cursor();
        // The cursor lives on the live screen; when scrolled back it moves
        // down the viewport and may leave it entirely.
        let cursor_vrow = cursor.row + offset;
        let now = self.fx.now();
        // Blink: fades when configured; an unfocused cursor never blinks.
        let blink = if view.focused { self.fx.blink_alpha(view.cursor_on, now) } else { 1.0 };
        let show_cursor = cursor.visible && cursor_vrow < rows && blink > 0.0;
        let block_cursor = show_cursor && view.focused && cursor.shape == CursorShape::Block;
        let cursor_wide = cursor_vrow < rows
            && term.visible_row(cursor_vrow).cells.get(cursor.col).is_some_and(|c| c.is_wide());
        let target_w = if cursor_wide { 2.0 * cw } else { cw };
        let target = [pad + cursor.col as f32 * cw, pad + cursor_vrow as f32 * ch, target_w, ch];
        // Glide: draw where the cursor is on its way, not where it ends.
        let (shown, settled) = if cursor.visible && cursor_vrow < rows {
            let (r, t) = self.fx.cursor(target, now);
            (r, t >= 1.0)
        } else {
            (target, true)
        };
        let [cursor_x, cursor_y, cursor_w, _] = shown;
        let cursor_color = to_f32(self.theme.cursor, blink);
        let sel_alpha = self.fx.selection_alpha(selection.is_some(), now);
        let selected = |r: usize, c: usize| {
            selection.is_some_and(|s: SelectionRange| {
                s.contains(Point::new(r as i64 - offset as i64, c))
            })
        };

        let inverts = block_cursor && settled && blink > 0.5;
        // Reuse rows from the last frame: at the same place, or moved by
        // scrolling (cached instances are relative to their row's top).
        let mut old = std::mem::take(&mut self.rows_cache);
        let mut cache: Vec<RowCache> = Vec::with_capacity(rows);
        // Rows to build (with their keys).
        let mut fresh: Vec<Option<RowKey>> = Vec::with_capacity(rows);
        let mut shift: isize = 0;
        for r in 0..rows {
            let row = term.visible_row(r);
            // Every column: a trimmed scrollback row may be shorter.
            let mut sel: Vec<bool> = if selection.is_some() {
                (0..cols).map(|c| selected(r, c)).collect()
            } else {
                Vec::new()
            };
            if !sel.iter().any(|s| *s) {
                sel.clear();
            }
            let alpha = if sel.is_empty() { 0 } else { sel_alpha.to_bits() };
            let inverted = (inverts && r == cursor_vrow).then_some(cursor.col);
            let fits = |c: &RowCache| {
                c.key.as_ref().is_some_and(|k| {
                    k.inverted == inverted
                        && k.sel_alpha == alpha
                        && k.selected == sel
                        && k.row == *row
                })
            };
            let near = [r as isize, r as isize + shift]
                .into_iter()
                .filter(|j| *j >= 0 && (*j as usize) < old.len())
                .map(|j| j as usize)
                .find(|j| fits(&old[*j]));
            let found = near.or_else(|| (0..old.len()).find(|j| fits(&old[*j])));
            match found {
                Some(j) => {
                    shift = j as isize - r as isize;
                    // Each old row is used once (identical rows, like blank
                    // ones, have their own entries).
                    cache.push(std::mem::take(&mut old[j]));
                    fresh.push(None);
                }
                None => {
                    cache.push(RowCache::default());
                    fresh.push(Some(RowKey {
                        row: row.clone(),
                        selected: sel,
                        sel_alpha: alpha,
                        inverted,
                    }));
                }
            }
        }
        drop(old);
        self.rows_cache = cache;
        self.stats.rows_rebuilt = fresh.iter().filter(|f| f.is_some()).count();
        let row_y = |r: usize| pad + r as f32 * ch;
        // Cached instances are stored relative to their row.
        fn place(out: &mut Vec<Instance>, from: &[Instance], dy: f32) {
            out.extend(from.iter().map(|i| {
                let mut i = *i;
                i.rect[1] += dy;
                i
            }));
        }
        fn relative(from: &[Instance], dy: f32) -> Vec<Instance> {
            from.iter()
                .map(|i| {
                    let mut i = *i;
                    i.rect[1] -= dy;
                    i
                })
                .collect()
        }

        // 1. Backgrounds, merged into runs of equal color. A new
        // selection fades in.
        for (r, f) in fresh.iter().enumerate() {
            let y = row_y(r);
            if f.is_none() {
                place(&mut self.instances, &self.rows_cache[r].bg, y);
                continue;
            }
            let start = self.instances.len();
            self.build_row_bg(term, r, cols, pad, cw, ch, sel_alpha, &selected);
            self.rows_cache[r].bg = relative(&self.instances[start..], y);
        }

        // Ripples and glow sit under the text.
        let center = [cursor_x + cursor_w / 2.0, cursor_y + ch / 2.0];
        for (c, p) in self.fx.ripples(center, now) {
            let max_r = ch * 3.0;
            let size = 2.0 * max_r + 4.0;
            let radius = max_r * (1.0 - (1.0 - p).powi(2));
            let color = to_f32(self.theme.cursor, 0.55 * (1.0 - p));
            self.push_effect(
                [c[0] - size / 2.0, c[1] - size / 2.0, size, size],
                color,
                KIND_RING,
                [radius, 1.5 + 1.5 * (1.0 - p), size],
            );
        }
        if show_cursor && self.fx.cfg.glow > 0.0 {
            let rad = ch * 0.7;
            let (qw, qh) = (cursor_w + 2.0 * rad, ch + 2.0 * rad);
            self.push_effect(
                [cursor_x - rad, cursor_y - rad, qw, qh],
                to_f32(self.theme.cursor, 0.55 * self.fx.cfg.glow * blink),
                KIND_GLOW,
                [rad, qw, qh],
            );
        }
        // Trail behind a gliding cursor.
        if show_cursor {
            for (r, a) in self.fx.trail(now) {
                self.push_rect(r[0], r[1], r[2], r[3], to_f32(self.theme.cursor, a * blink));
            }
        }

        // 2. Block cursor sits under the glyph.
        if block_cursor {
            self.push_rect(cursor_x, cursor_y, cursor_w, ch, cursor_color);
        }

        self.bg_end = self.instances.len() as u32;

        // 3. Glyphs and decorations.
        for (r, f) in fresh.iter_mut().enumerate() {
            let y = row_y(r);
            let Some(k) = f.take() else {
                place(&mut self.instances, &self.rows_cache[r].fg, y);
                continue;
            };
            let start = self.instances.len();
            if !self.build_row_fg(queue, term, r, cols, pad, cw, ch, &m, k.inverted, &selected) {
                // The atlas overflowed: everything is rebuilt.
                self.rows_cache.clear();
                return false;
            }
            self.rows_cache[r].fg = relative(&self.instances[start..], y);
            self.rows_cache[r].key = Some(k);
        }

        // 4. Non-block cursors draw over the text.
        if show_cursor && !block_cursor {
            let c = cursor_color;
            let t = (m.stroke * 2.0).max(1.0).round();
            if !view.focused {
                self.push_rect(cursor_x, cursor_y, cursor_w, 1.0, c);
                self.push_rect(cursor_x, cursor_y + ch - 1.0, cursor_w, 1.0, c);
                self.push_rect(cursor_x, cursor_y, 1.0, ch, c);
                self.push_rect(cursor_x + cursor_w - 1.0, cursor_y, 1.0, ch, c);
            } else if cursor.shape == CursorShape::Bar {
                self.push_rect(cursor_x, cursor_y, t, ch, c);
            } else {
                self.push_rect(cursor_x, cursor_y + ch - t, cursor_w, t, c);
            }
        }
        // 5. Scroll position indicator while viewing history.
        let history = term.scrollback().len();
        if offset > 0 && history > 0 {
            let track = rows as f32 * ch;
            let total = (history + rows) as f32;
            let thumb = (track * rows as f32 / total).max(ch);
            let top = (track - thumb) * (1.0 - offset as f32 / history as f32);
            let w = (pad / 2.0).clamp(2.0, 4.0);
            let x = view.width as f32 - w - 1.0;
            self.push_solid(x, pad + top, w, thumb, self.theme.selection_bg);
        }
        true
    }

    /// Cell colors, with the selection highlight applied.
    fn colors(&self, attrs: &Attrs, selected: bool) -> (Rgb, Option<Rgb>) {
        let (fg, bg) = self.theme.cell_colors(attrs);
        if selected {
            (self.theme.selection_fg.unwrap_or(fg), Some(self.theme.selection_bg))
        } else {
            (fg, bg)
        }
    }

    /// Backgrounds of visible row `r`, merged into runs of equal color.
    #[allow(clippy::too_many_arguments)]
    fn build_row_bg(
        &mut self,
        term: &Terminal,
        r: usize,
        cols: usize,
        pad: f32,
        cw: f32,
        ch: f32,
        sel_alpha: f32,
        selected: &dyn Fn(usize, usize) -> bool,
    ) {
        let row = term.visible_row(r);
        let y = pad + r as f32 * ch;
        let mut run: Option<(usize, Rgb, bool)> = None;
        // Every column: cells past the end of a trimmed scrollback row are
        // blank, and may be selected.
        let blank = seraphim_terminal::Cell::default();
        let n = cols;
        for c in 0..n {
            let cell = row.cells.get(c).unwrap_or(&blank);
            let sel = selected(r, c);
            let (_, bg) = self.colors(&cell.attrs, sel);
            match (run, bg) {
                (Some((_, rc, rs)), Some(b)) if rc == b && rs == sel => {}
                _ => {
                    if let Some((start, color, s)) = run.take() {
                        let a = if s { sel_alpha } else { 1.0 };
                        let w = (c - start) as f32 * cw;
                        self.push_rect(pad + start as f32 * cw, y, w, ch, to_f32(color, a));
                    }
                    run = bg.map(|b| (c, b, sel));
                }
            }
        }
        if let Some((start, color, s)) = run {
            let a = if s { sel_alpha } else { 1.0 };
            let w = (n - start) as f32 * cw;
            self.push_rect(pad + start as f32 * cw, y, w, ch, to_f32(color, a));
        }
    }

    /// Glyphs and decorations of visible row `r`. `inverted`: the column
    /// of a block cursor drawing its text in the cursor-text color.
    /// Returns false if the glyph atlas overflowed.
    #[allow(clippy::too_many_arguments)]
    fn build_row_fg(
        &mut self,
        queue: &wgpu::Queue,
        term: &Terminal,
        r: usize,
        cols: usize,
        pad: f32,
        cw: f32,
        ch: f32,
        m: &CellMetrics,
        inverted: Option<usize>,
        selected: &dyn Fn(usize, usize) -> bool,
    ) -> bool {
        let row = term.visible_row(r);
        let y = pad + r as f32 * ch;
        let n = row.cells.len().min(cols);
        for (c, cell) in row.cells[..n].iter().enumerate() {
            if cell.is_wide_spacer() {
                continue;
            }
            let x = pad + c as f32 * cw;
            let width = if cell.is_wide() { 2.0 * cw } else { cw };
            let (mut fg, _) = self.colors(&cell.attrs, selected(r, c));
            // Text inverts under the block only once it has arrived and
            // is mostly opaque.
            let under_cursor = inverted == Some(c);
            if under_cursor {
                fg = self.theme.cursor_text;
            }
            let flags = cell.attrs.flags;
            // Unicode image placeholders draw as images (images.rs).
            let placeholder = cell.ch == seraphim_terminal::graphics::PLACEHOLDER;
            if cell.ch != ' ' && !placeholder && !flags.contains(Flags::HIDDEN) {
                let style =
                    FontStyle::new(flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC));
                let marks = row.combining(c);
                let ok = if !marks.is_empty() {
                    let mut text = String::with_capacity(8);
                    text.push(cell.ch);
                    text.extend(marks.iter());
                    self.push_cluster(queue, &text, style, x, y, width, fg)
                } else if is_private_use(cell.ch) {
                    // Nerd Font icons may spill into a following blank.
                    // (Past the end of a trimmed scrollback row is blank.)
                    let next_blank = c + 1 < cols
                        && row.cells.get(c + 1).is_none_or(|n| n.ch == ' ' && !n.is_wide_spacer())
                        && row.combining(c + 1).is_empty();
                    // Powerline separators (U+E0A0-E0D7) must stay one
                    // cell wide so segment edges meet their backgrounds.
                    let powerline = matches!(cell.ch as u32, 0xE0A0..=0xE0D7);
                    let box_w =
                        if next_blank && !cell.is_wide() && !powerline { 2.0 * cw } else { width };
                    self.push_symbol(queue, cell.ch, style, x, y, box_w, fg)
                } else if is_ignorable(cell.ch) {
                    true
                } else {
                    self.push_glyph(queue, cell.ch, style, x, y, width, fg)
                };
                if !ok {
                    return false;
                }
            }
            let stroke = m.stroke;
            if flags.intersects(Flags::ALL_UNDERLINES) {
                let uc = self.theme.underline_color(term.underline_color(&cell.attrs), fg);
                let uy = y + m.underline_y;
                // Patterns are aligned to absolute x so they continue
                // seamlessly across cells.
                let pattern = |r: &mut Self, on: f32, off: f32, dy: f32| {
                    let period = on + off;
                    let mut sx = x - ((x - pad) % period);
                    while sx < x + width {
                        let a = sx.max(x);
                        let b = (sx + on).min(x + width);
                        if b > a {
                            r.push_solid(a, uy + dy, b - a, stroke, uc);
                        }
                        sx += period;
                    }
                };
                if flags.contains(Flags::CURLY_UNDERLINE) {
                    // A wave: alternating raised and lowered segments.
                    let seg = (cw / 4.0).max(1.0);
                    pattern(self, seg, seg, -stroke);
                    let mut sx = x - ((x - pad) % (2.0 * seg)) + seg;
                    while sx < x + width {
                        let a = sx.max(x);
                        let b = (sx + seg).min(x + width);
                        if b > a {
                            self.push_solid(a, uy, b - a, stroke, uc);
                        }
                        sx += 2.0 * seg;
                    }
                } else if flags.contains(Flags::DOTTED_UNDERLINE) {
                    let dot = stroke.max(1.0);
                    pattern(self, dot, dot, 0.0);
                } else if flags.contains(Flags::DASHED_UNDERLINE) {
                    let dash = (cw * 0.5).round().max(2.0);
                    pattern(self, dash, (cw * 0.25).round().max(1.0), 0.0);
                } else {
                    self.push_solid(x, uy, width, stroke, uc);
                    if flags.contains(Flags::DOUBLE_UNDERLINE) {
                        self.push_solid(x, (uy - 2.0 * stroke).max(y), width, stroke, uc);
                    }
                }
            }
            if flags.contains(Flags::OVERLINE) {
                self.push_solid(x, y, width, stroke, fg);
            }
            if flags.contains(Flags::STRIKETHROUGH) {
                self.push_solid(x, y + m.strike_y, width, stroke, fg);
            }
        }
        true
    }

    fn push_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        self.instances.push(Instance {
            rect: [x, y, w, h],
            uv: [0.0; 4],
            color,
            kind: KIND_SOLID,
            params: [0.0; 3],
        });
    }

    fn push_effect(&mut self, rect: [f32; 4], color: [f32; 4], kind: u32, params: [f32; 3]) {
        // uv spans the whole atlas so the shader sees 0..1 across the quad.
        let uv = [0.0, 0.0, ATLAS_SIZE as f32, ATLAS_SIZE as f32];
        self.instances.push(Instance { rect, uv, color, kind, params });
    }

    fn push_solid(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
        self.instances.push(Instance {
            rect: [x, y, w, h],
            uv: [0.0; 4],
            color: to_f32(color, 1.0),
            kind: KIND_SOLID,
            params: [0.0; 3],
        });
    }

    /// The cached atlas entry for `key`, uploading it on first use.
    /// `Err` means the atlas is full.
    fn cached(&mut self, queue: &wgpu::Queue, key: GlyphKey) -> Result<Option<CachedGlyph>, ()> {
        if let Some(g) = self.glyphs.get(&key) {
            return Ok(*g);
        }
        let g = self.upload_glyph(queue, key).ok_or(())?;
        self.glyphs.insert(key, g);
        Ok(g)
    }

    /// Returns false if the atlas is full.
    #[allow(clippy::too_many_arguments)]
    fn push_glyph(
        &mut self,
        queue: &wgpu::Queue,
        ch: char,
        style: FontStyle,
        x: f32,
        y: f32,
        cell_w: f32,
        fg: Rgb,
    ) -> bool {
        let Some(key) = self.fonts.lookup(ch, style) else { return true };
        self.push_key(queue, key, x, y, 0.0, 0.0, cell_w, fg)
    }

    /// Draw `key` with its pen at (`x + dx`, baseline - `dy`) in the cell at
    /// (`x`, `y`). Color glyphs larger than the cell are fitted to it.
    #[allow(clippy::too_many_arguments)]
    fn push_key(
        &mut self,
        queue: &wgpu::Queue,
        key: GlyphKey,
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        cell_w: f32,
        fg: Rgb,
    ) -> bool {
        let Ok(glyph) = self.cached(queue, key) else { return false };
        let Some(g) = glyph else { return true };
        let m = self.fonts.metrics();
        let (mut w, mut h) = (g.w as f32, g.h as f32);
        let (mut gx, mut gy) =
            ((x + dx).round() + g.left as f32, (y + m.baseline - dy).round() - g.top as f32);
        if g.color && (h > m.height || w > cell_w) {
            // Emoji bitmaps come in fixed strikes (e.g. 136px): fit to cell.
            let s = (m.height / h).min(cell_w / w);
            w = (w * s).round();
            h = (h * s).round();
            gx = x + ((cell_w - w) / 2.0).round();
            gy = y + ((m.height - h) / 2.0).round();
        }
        self.instances.push(Instance {
            rect: [gx, gy, w, h],
            uv: [g.rect.x as f32, g.rect.y as f32, g.rect.w as f32, g.rect.h as f32],
            color: to_f32(fg, 1.0),
            kind: if g.color { KIND_COLOR } else { KIND_MASK },
            params: [0.0; 3],
        });
        true
    }

    /// Draw a multi-codepoint grapheme cluster (base + combining marks, or
    /// an emoji sequence) as the font intends.
    #[allow(clippy::too_many_arguments)]
    fn push_cluster(
        &mut self,
        queue: &wgpu::Queue,
        text: &str,
        style: FontStyle,
        x: f32,
        y: f32,
        cell_w: f32,
        fg: Rgb,
    ) -> bool {
        let emoji = is_emoji_cluster(text);
        if !emoji {
            // A precomposed character (é for e + U+0301) in the cell's own
            // font is the crispest result.
            let mut nfc = text.nfc();
            if let (Some(c), None) = (nfc.next(), nfc.next()) {
                if let Some(key) = self.fonts.lookup(c, style) {
                    return self.push_key(queue, key, x, y, 0.0, 0.0, cell_w, fg);
                }
            }
        }
        let Some(face) = self.fonts.lookup_cluster(text, style, emoji) else { return true };
        let k = (face, text.to_owned());
        let glyphs = match self.shaped.get(&k) {
            Some(g) => g.clone(),
            None => {
                let g = self.fonts.shape(face, text);
                if self.shaped.len() > 4096 {
                    self.shaped.clear();
                }
                self.shaped.insert(k, g.clone());
                g
            }
        };
        let bold = style == FontStyle::Bold || style == FontStyle::BoldItalic;
        for g in glyphs {
            let key = GlyphKey { face, glyph: g.glyph, bold, size: 0 };
            if !self.push_key(queue, key, x, y, g.x, g.y, cell_w, fg) {
                return false;
            }
        }
        true
    }

    /// Draw a private-use (Nerd Font) icon scaled to fill `box_w` x cell
    /// height, centered. Icon fonts' glyphs are sized and positioned for
    /// their own metrics, not ours, so drawing them as-is makes them too
    /// small or offset (sometimes nearly out of the cell).
    #[allow(clippy::too_many_arguments)]
    fn push_symbol(
        &mut self,
        queue: &wgpu::Queue,
        ch: char,
        style: FontStyle,
        x: f32,
        y: f32,
        box_w: f32,
        fg: Rgb,
    ) -> bool {
        let Some(key) = self.fonts.lookup(ch, style) else { return true };
        let Ok(base) = self.cached(queue, key) else { return false };
        let Some(b) = base else { return true };
        let m = self.fonts.metrics();
        let (w, h) = (b.w as f32, b.h as f32);
        // Powerline glyphs fill the full cell height (no margin) so they join
        // seamlessly with the rows of a segmented prompt.
        let powerline = matches!(ch as u32, 0xE0A0..=0xE0D7);
        let margin = if powerline { 0.0 } else { 1.0 };
        // The separators (arrows, half circles, slants: U+E0B0-E0BF) are
        // shapes, stretched to fill the cell exactly as other terminals
        // draw them; otherwise their aspect ratio leaves a gap.
        let separator = matches!(ch as u32, 0xE0B0..=0xE0BF);
        let s = if separator {
            (m.height / h).clamp(0.25, 4.0)
        } else {
            ((box_w - margin) / w).min((m.height - margin) / h).clamp(0.25, 4.0)
        };
        let key = if (s - 1.0).abs() > 0.05 {
            let size = (self.fonts.size_px() * s * 4.0).round().clamp(4.0, u16::MAX as f32) as u16;
            GlyphKey { size, ..key }
        } else {
            key
        };
        let Ok(glyph) = self.cached(queue, key) else { return false };
        let Some(g) = glyph else { return true };
        let (w, h) = (g.w as f32, g.h as f32);
        let rect = if separator {
            [x, y, box_w, m.height]
        } else {
            [x + ((box_w - w) / 2.0).round(), y + ((m.height - h) / 2.0).round(), w, h]
        };
        self.instances.push(Instance {
            rect,
            uv: [g.rect.x as f32, g.rect.y as f32, g.rect.w as f32, g.rect.h as f32],
            color: to_f32(fg, 1.0),
            kind: if g.color { KIND_COLOR } else { KIND_MASK },
            params: [0.0; 3],
        });
        true
    }

    /// Rasterize and upload. Outer `None`: atlas full. Inner `None`: the
    /// glyph has no visible pixels.
    fn upload_glyph(&mut self, queue: &wgpu::Queue, key: GlyphKey) -> Option<Option<CachedGlyph>> {
        self.stats.glyphs_rasterized += 1;
        let Some(raster) = self.fonts.rasterize(key) else { return Some(None) };
        if raster.width == 0 || raster.height == 0 {
            return Some(None);
        }
        let (w, h) = (raster.width, raster.height);
        let (rect, texture, bytes_per_pixel, pixels);
        if raster.color {
            // Color glyphs taller than the cell are drawn fitted to it (see
            // push_key). swash already scales bitmap strikes to the font
            // size, but a glyph that still comes out too tall is shrunk to
            // the cell height here, averaging every source pixel: the GPU
            // then draws it at about 1:1, and it takes no more of the
            // (smaller) color atlas than it needs.
            let fit = self.fonts.metrics().height / h as f32;
            let (sw, sh) = if fit < 1.0 {
                (((w as f32 * fit).round() as u32).max(1), ((h as f32 * fit).round() as u32).max(1))
            } else {
                (w, h)
            };
            pixels = if (sw, sh) != (w, h) {
                std::borrow::Cow::Owned(shrink_rgba(&raster.pixels, w, h, sw, sh))
            } else {
                std::borrow::Cow::Borrowed(&raster.pixels[..])
            };
            if self.color_texture.is_none() {
                self.create_color_atlas();
            }
            rect = self.color_atlas.allocate(sw, sh)?;
            texture = self.color_texture.as_ref().expect("created above");
            bytes_per_pixel = 4;
        } else {
            rect = self.atlas.allocate(w, h)?;
            texture = &self.mask_texture;
            bytes_per_pixel = 1;
            pixels = std::borrow::Cow::Borrowed(&raster.pixels[..]);
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: rect.x, y: rect.y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_pixel * rect.w),
                rows_per_image: Some(rect.h),
            },
            wgpu::Extent3d { width: rect.w, height: rect.h, depth_or_array_layers: 1 },
        );
        Some(Some(CachedGlyph {
            rect,
            w,
            h,
            left: raster.left,
            top: raster.top,
            color: raster.color,
        }))
    }
}

/// A square glyph atlas texture.
fn atlas_texture(
    device: &wgpu::Device,
    label: &str,
    size: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// The cell pipeline's bind group: globals, the mask and color atlases and
/// their samplers (see shader.wgsl).
fn cells_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    globals: &wgpu::Buffer,
    [mask, color]: [&wgpu::TextureView; 2],
    [nearest, linear]: [&wgpu::Sampler; 2],
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("cells"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(mask) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(nearest) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(linear) },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(color),
            },
        ],
    })
}

/// Shrink a straight-alpha RGBA image from `w` x `h` to `nw` x `nh` (each no
/// larger), each output pixel the area-weighted average of the source
/// pixels it covers. Colors are averaged weighted by alpha, so transparent
/// pixels don't darken the edges.
fn shrink_rgba(src: &[u8], w: u32, h: u32, nw: u32, nh: u32) -> Vec<u8> {
    // Source spans (first pixel, weights) covered by each output column/row.
    fn spans(n: u32, out: u32) -> Vec<(usize, Vec<f32>)> {
        let step = n as f32 / out as f32;
        (0..out)
            .map(|o| {
                let (a, b) = (o as f32 * step, ((o + 1) as f32 * step).min(n as f32));
                let first = a.floor() as usize;
                let last = (b.ceil() as usize).min(n as usize);
                let weights = (first..last)
                    .map(|i| (b.min(i as f32 + 1.0) - a.max(i as f32)).max(0.0))
                    .collect();
                (first, weights)
            })
            .collect()
    }
    let (xs, ys) = (spans(w, nw), spans(h, nh));
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    for (y0, wy) in &ys {
        for (x0, wx) in &xs {
            let mut acc = [0.0f32; 4];
            let mut area = 0.0;
            for (dy, &fy) in wy.iter().enumerate() {
                let row = (y0 + dy) * w as usize;
                for (dx, &fx) in wx.iter().enumerate() {
                    let p = &src[(row + x0 + dx) * 4..][..4];
                    let f = fx * fy;
                    let a = p[3] as f32 * f;
                    acc[0] += p[0] as f32 * a;
                    acc[1] += p[1] as f32 * a;
                    acc[2] += p[2] as f32 * a;
                    acc[3] += a;
                    area += f;
                }
            }
            let alpha = acc[3] / area.max(f32::EPSILON);
            let c = |v: f32| {
                if acc[3] > 0.0 {
                    (v / acc[3]).round().clamp(0.0, 255.0) as u8
                } else {
                    0
                }
            };
            out.extend([c(acc[0]), c(acc[1]), c(acc[2]), alpha.round().clamp(0.0, 255.0) as u8]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::shrink_rgba;

    #[test]
    fn shrinking_averages_by_area_and_alpha() {
        // 2x2 -> 1x1: one opaque red pixel and three transparent ones give
        // pure red at a quarter of the alpha (no darkening from the
        // transparent pixels' black).
        let src = [255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(shrink_rgba(&src, 2, 2, 1, 1), [255, 0, 0, 64]);
        // 3x1 -> 2x1: each output pixel covers 1.5 source pixels.
        let src = [0, 0, 0, 255, 90, 90, 90, 255, 180, 180, 180, 255];
        assert_eq!(shrink_rgba(&src, 3, 1, 2, 1), [30, 30, 30, 255, 150, 150, 150, 255]);
        // Same size: unchanged.
        let src: Vec<u8> = (0..36).collect();
        let opaque: Vec<u8> =
            src.iter().enumerate().map(|(i, &v)| if i % 4 == 3 { 255 } else { v }).collect();
        assert_eq!(shrink_rgba(&opaque, 3, 3, 3, 3), opaque);
    }
}
