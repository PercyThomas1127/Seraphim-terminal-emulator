//! Offscreen rendering for tests, screenshots and CI.

use std::path::Path;

use seraphim_terminal::Terminal;

use crate::{Fonts, Renderer, Theme, View};

/// The format used for offscreen targets: non-sRGB, matching the window
/// path, so screenshots look exactly like the window.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// An RGBA8 image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    pub fn save_png(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = png::Encoder::new(file, self.width, self.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(std::io::Error::other)?;
        w.write_image_data(&self.rgba).map_err(std::io::Error::other)?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct HeadlessError(pub String);

impl std::fmt::Display for HeadlessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for HeadlessError {}

/// A device plus renderer with no window.
pub struct Headless {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: Renderer,
    pub adapter_info: wgpu::AdapterInfo,
}

impl Headless {
    /// Acquire any adapter (hardware or software, e.g. lavapipe).
    pub fn new(fonts: Fonts, theme: Theme) -> Result<Headless, HeadlessError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
        }))
        .map_err(|e| HeadlessError(format!("no GPU adapter: {e}")))?;
        let adapter_info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("seraphim headless"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| HeadlessError(format!("request_device: {e}")))?;
        let renderer = Renderer::new(&device, FORMAT, fonts, theme);
        Ok(Headless { device, queue, renderer, adapter_info })
    }

    /// Pixel size that exactly fits the terminal grid plus padding.
    pub fn size_for(&self, term: &Terminal, padding: f32) -> (u32, u32) {
        let m = self.renderer.cell_metrics();
        (
            (term.cols() as f32 * m.width + 2.0 * padding.round()) as u32,
            (term.rows() as f32 * m.height + 2.0 * padding.round()) as u32,
        )
    }

    /// Render `term` and read the pixels back.
    pub fn render(&mut self, term: &Terminal, view: View) -> Image {
        self.render_with(term, view, &mut |_| {})
    }

    /// Like [`Headless::render`], with `background` drawing into the pass
    /// before the text (e.g. visual layers).
    pub fn render_with(
        &mut self,
        term: &Terminal,
        view: View,
        background: &mut dyn FnMut(&mut wgpu::RenderPass<'_>),
    ) -> Image {
        self.render_with_encoder(term, view, &mut |_| {}, background)
    }

    /// Like [`Headless::render_with`], with `pre` recording offscreen work
    /// (e.g. a visual engine's compositing) before the frame's pass.
    pub fn render_with_encoder(
        &mut self,
        term: &Terminal,
        view: View,
        pre: &mut dyn FnMut(&mut wgpu::CommandEncoder),
        background: &mut dyn FnMut(&mut wgpu::RenderPass<'_>),
    ) -> Image {
        self.renderer.prepare(&self.device, &self.queue, term, view);
        self.frame(view.width, view.height, pre, &mut |r, pass| {
            background(pass);
            r.render(pass);
        })
    }

    /// Several terminals in one frame, each in its own renderer slot and
    /// viewport `[x, y, w, h]` (pixels); `view` sizes are the panes'.
    pub fn render_panes(
        &mut self,
        width: u32,
        height: u32,
        panes: &[(u64, &Terminal, View, [f32; 4])],
    ) -> Image {
        for (slot, term, view, _) in panes {
            self.renderer.select(&self.device, *slot);
            self.renderer.prepare(&self.device, &self.queue, term, *view);
        }
        let order: Vec<(u64, [f32; 4])> = panes.iter().map(|(s, _, _, r)| (*s, *r)).collect();
        self.frame_multi(width, height, &order)
    }

    fn frame_multi(&mut self, width: u32, height: u32, panes: &[(u64, [f32; 4])]) -> Image {
        // Renderer::render reads the selected slot only, so record each
        // pane with its slot selected; selection between draws in one pass
        // is fine because GPU resources live in the slots.
        let device = self.device.clone();
        self.frame(width, height, &mut |_| {}, &mut |r, pass| {
            for (slot, [x, y, w, h]) in panes {
                r.select(&device, *slot);
                pass.set_viewport(*x, *y, *w, *h, 0.0, 1.0);
                r.render(pass);
            }
        })
    }

    fn frame(
        &mut self,
        width: u32,
        height: u32,
        pre: &mut dyn FnMut(&mut wgpu::CommandEncoder),
        draw: &mut dyn FnMut(&mut Renderer, &mut wgpu::RenderPass<'_>),
    ) -> Image {
        let (width, height) = (width.max(1), height.max(1));
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        pre(&mut encoder);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.renderer.clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            draw(&mut self.renderer, &mut pass);
        }

        // Rows in a texture-to-buffer copy must be 256-byte aligned.
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let (tx, rx) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .expect("device poll");
        rx.recv().expect("map callback").expect("buffer map");
        let data = buffer.get_mapped_range(..).expect("mapped range");
        let mut rgba = Vec::with_capacity((unpadded * height) as usize);
        for row in data.chunks(padded as usize) {
            rgba.extend_from_slice(&row[..unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        Image { width, height, rgba }
    }
}
