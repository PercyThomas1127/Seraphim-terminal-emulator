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
pub mod font;
pub mod headless;
mod theme;

use std::collections::HashMap;

use seraphim_terminal::{Attrs, CursorShape, Flags, Point, SelectionRange, Terminal};

use atlas::{Atlas, AtlasRect, ATLAS_SIZE};
use font::GlyphKey;
pub use font::{CellMetrics, FontConfig, FontError, FontStyle, Fonts};
use theme::to_f32;
pub use theme::{Rgb, Theme};

/// One quad. Layout must match `Instance` in `shader.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    kind: u32,
    _pad: [u32; 3],
}

// SAFETY: plain-old-data, repr(C), no padding beyond the explicit field.
unsafe impl bytemuck::Zeroable for Instance {}
unsafe impl bytemuck::Pod for Instance {}

const KIND_SOLID: u32 = 0;
const KIND_MASK: u32 = 1;
const KIND_COLOR: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct Globals {
    screen: [f32; 2],
    atlas: [f32; 2],
}
unsafe impl bytemuck::Zeroable for Globals {}
unsafe impl bytemuck::Pod for Globals {}

/// Per-frame view parameters.
#[derive(Clone, Copy, Debug)]
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
    rect: AtlasRect,
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
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    atlas_texture: wgpu::Texture,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    instance_count: u32,
    atlas: Atlas,
    glyphs: HashMap<GlyphKey, Option<CachedGlyph>>,
    fonts: Fonts,
    /// The configured theme.
    base_theme: Theme,
    /// `base_theme` plus the terminal's color overrides, for this frame.
    theme: Theme,
    applied_overrides: seraphim_terminal::palette::ColorOverrides,
    instances: Vec<Instance>,
    stats: FrameStats,
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
        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
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
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cells"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&linear_sampler),
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cells"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes =
            wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32];
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
        Renderer {
            pipeline,
            bind_group,
            globals,
            atlas_texture,
            instance_buffer,
            instance_capacity,
            instance_count: 0,
            atlas: Atlas::new(ATLAS_SIZE),
            glyphs: HashMap::new(),
            fonts,
            base_theme: theme.clone(),
            theme,
            applied_overrides: Default::default(),
            instances: Vec::new(),
            stats: FrameStats::default(),
        }
    }

    fn make_instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
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
        self.theme = theme;
        self.applied_overrides = Default::default();
    }

    /// Replace fonts (e.g. on scale-factor or font-size change).
    pub fn set_fonts(&mut self, fonts: Fonts) {
        self.fonts = fonts;
        self.glyphs.clear();
        self.atlas.clear();
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
        self.stats = FrameStats::default();
        self.build_instances(queue, term, &view);
        if self.instances.len() > self.instance_capacity {
            self.instance_capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = Self::make_instance_buffer(device, self.instance_capacity);
        }
        queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&self.instances));
        let globals = Globals {
            screen: [view.width.max(1) as f32, view.height.max(1) as f32],
            atlas: [ATLAS_SIZE as f32, ATLAS_SIZE as f32],
        };
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        self.instance_count = self.instances.len() as u32;
        self.stats.instances = self.instances.len();
    }

    /// Record the draw into an existing render pass.
    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.instance_count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..4, 0..self.instance_count);
    }

    fn build_instances(&mut self, queue: &wgpu::Queue, term: &Terminal, view: &View) {
        // If the atlas fills mid-frame, clear it and rebuild once.
        for attempt in 0..2 {
            if self.try_build(queue, term, view) {
                return;
            }
            self.glyphs.clear();
            self.atlas.clear();
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
        }
        let m = self.fonts.metrics();
        let (cw, ch) = (m.width, m.height);
        let pad = view.padding.round();
        let (rows, cols) = (term.rows(), term.cols());
        let offset = term.display_offset();
        let selection = term.selection_range();
        let cursor = term.cursor();
        // The cursor lives on the live screen; when scrolled back it moves
        // down the viewport and may leave it entirely.
        let cursor_vrow = cursor.row + offset;
        let show_cursor = cursor.visible && cursor_vrow < rows && (view.cursor_on || !view.focused);
        let block_cursor = show_cursor && view.focused && cursor.shape == CursorShape::Block;
        let cursor_wide = show_cursor
            && term.visible_row(cursor_vrow).cells.get(cursor.col).is_some_and(|c| c.is_wide());
        let cursor_w = if cursor_wide { 2.0 * cw } else { cw };
        let cursor_x = pad + cursor.col as f32 * cw;
        let cursor_y = pad + cursor_vrow as f32 * ch;
        let selected = |r: usize, c: usize| {
            selection.is_some_and(|s: SelectionRange| {
                s.contains(Point::new(r as i64 - offset as i64, c))
            })
        };

        // 1. Backgrounds, merged into runs of equal color.
        for r in 0..rows {
            let row = term.visible_row(r);
            let y = pad + r as f32 * ch;
            let mut run: Option<(usize, Rgb)> = None;
            let n = row.cells.len().min(cols);
            for (c, cell) in row.cells[..n].iter().enumerate() {
                let (_, bg) = self.colors(&cell.attrs, selected(r, c));
                match (run, bg) {
                    (Some((_, rc)), Some(b)) if rc == b => {}
                    _ => {
                        if let Some((start, color)) = run.take() {
                            self.push_solid(
                                pad + start as f32 * cw,
                                y,
                                (c - start) as f32 * cw,
                                ch,
                                color,
                            );
                        }
                        run = bg.map(|b| (c, b));
                    }
                }
            }
            if let Some((start, color)) = run {
                self.push_solid(pad + start as f32 * cw, y, (n - start) as f32 * cw, ch, color);
            }
        }

        // 2. Block cursor sits under the glyph.
        if block_cursor {
            self.push_solid(cursor_x, cursor_y, cursor_w, ch, self.theme.cursor);
        }

        // 3. Glyphs and decorations.
        for r in 0..rows {
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
                let under_cursor = block_cursor && r == cursor_vrow && c == cursor.col;
                if under_cursor {
                    fg = self.theme.cursor_text;
                }
                let flags = cell.attrs.flags;
                if cell.ch != ' ' && !flags.contains(Flags::HIDDEN) {
                    let style =
                        FontStyle::new(flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC));
                    if !self.push_glyph(queue, cell.ch, style, x, y, width, fg) {
                        return false;
                    }
                    if !row.combining(c).is_empty() {
                        for &mark in row.combining(c) {
                            if !self.push_glyph(queue, mark, style, x, y, width, fg) {
                                return false;
                            }
                        }
                    }
                }
                let stroke = m.stroke;
                if flags.intersects(Flags::ALL_UNDERLINES) {
                    let uc = self.theme.underline_color(&cell.attrs, fg);
                    let uy = y + m.underline_y;
                    self.push_solid(x, uy, width, stroke, uc);
                    if flags.contains(Flags::DOUBLE_UNDERLINE) {
                        self.push_solid(x, (uy - 2.0 * stroke).max(y), width, stroke, uc);
                    }
                    if flags.contains(Flags::CURLY_UNDERLINE) {
                        // Approximate a wave with alternating short dashes.
                        let seg = (cw / 4.0).max(1.0);
                        let mut sx = x;
                        let mut up = true;
                        while sx < x + width {
                            let dy = if up { -stroke } else { 0.0 };
                            self.push_solid(sx, uy + dy, seg.min(x + width - sx), stroke, uc);
                            sx += seg;
                            up = !up;
                        }
                    }
                }
                if flags.contains(Flags::STRIKETHROUGH) {
                    self.push_solid(x, y + m.strike_y, width, stroke, fg);
                }
            }
        }

        // 4. Non-block cursors draw over the text.
        if show_cursor && !block_cursor {
            let c = self.theme.cursor;
            let t = (m.stroke * 2.0).max(1.0).round();
            if !view.focused {
                self.push_solid(cursor_x, cursor_y, cursor_w, 1.0, c);
                self.push_solid(cursor_x, cursor_y + ch - 1.0, cursor_w, 1.0, c);
                self.push_solid(cursor_x, cursor_y, 1.0, ch, c);
                self.push_solid(cursor_x + cursor_w - 1.0, cursor_y, 1.0, ch, c);
            } else if cursor.shape == CursorShape::Bar {
                self.push_solid(cursor_x, cursor_y, t, ch, c);
            } else {
                self.push_solid(cursor_x, cursor_y + ch - t, cursor_w, t, c);
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

    fn push_solid(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
        self.instances.push(Instance {
            rect: [x, y, w, h],
            uv: [0.0; 4],
            color: to_f32(color, 1.0),
            kind: KIND_SOLID,
            _pad: [0; 3],
        });
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
        let glyph = match self.glyphs.get(&key) {
            Some(g) => *g,
            None => {
                let Some(g) = self.upload_glyph(queue, key) else { return false };
                self.glyphs.insert(key, g);
                g
            }
        };
        let Some(g) = glyph else { return true };
        let m = self.fonts.metrics();
        let (mut w, mut h) = (g.rect.w as f32, g.rect.h as f32);
        let (mut gx, mut gy) = (x + g.left as f32, y + m.baseline - g.top as f32);
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
            _pad: [0; 3],
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
        let rect = self.atlas.allocate(raster.width, raster.height)?;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: rect.x, y: rect.y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &raster.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * raster.width),
                rows_per_image: Some(raster.height),
            },
            wgpu::Extent3d { width: raster.width, height: raster.height, depth_or_array_layers: 1 },
        );
        Some(Some(CachedGlyph { rect, left: raster.left, top: raster.top, color: raster.color }))
    }
}
