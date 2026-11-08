//! The windowed front end: winit window + wgpu surface + one session.
//!
//! Threading: the event loop thread owns the window, surface and renderer.
//! The session's I/O thread never touches them; it only pokes the loop via
//! an `EventLoopProxy`, and the loop redraws (coalesced, vsync-paced) when
//! it gets around to it. No frames are rendered while nothing changes.

use std::sync::Arc;

use seraphim_events::EventBus;
use seraphim_render::{FontConfig, Fonts, Renderer, Theme, View};
use seraphim_session::{Command, Notifier, Session, SessionConfig};
use seraphim_terminal::input::{encode_focus, encode_key, encode_paste, Key, Mods};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, Ime, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_size: f32,
    pub cols: u16,
    pub rows: u16,
    /// Program to run; `None` runs the user's shell.
    pub command: Option<Command>,
    /// Logical padding in pixels.
    pub padding: f32,
}

impl Default for WindowOptions {
    fn default() -> Self {
        WindowOptions {
            font_family: None,
            font_size: 11.0,
            cols: 100,
            rows: 30,
            command: None,
            padding: 6.0,
        }
    }
}

#[derive(Debug)]
enum UserEvent {
    /// Terminal state changed (output, exit).
    Wake,
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    renderer: Renderer,
    session: Session,
    _bus: EventBus,
    modifiers: ModifiersState,
    focused: bool,
    scale: f64,
    title: String,
    last_generation: u64,
}

struct App {
    options: WindowOptions,
    proxy: EventLoopProxy<UserEvent>,
    active: Option<Active>,
    error: Option<String>,
    exit_code: i32,
}

pub fn run(options: WindowOptions) -> Result<i32, String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(|e| format!("cannot open a window: {e}"))?;
    let mut app =
        App { options, proxy: event_loop.create_proxy(), active: None, error: None, exit_code: 0 };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    match app.error {
        Some(e) => Err(e),
        None => Ok(app.exit_code),
    }
}

fn font_px(points: f32, scale: f64) -> f32 {
    // 1pt = 1/72in at 96 dpi logical.
    (points * 96.0 / 72.0 * scale as f32).round()
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Active, String> {
        let o = &self.options;
        let scale = event_loop.primary_monitor().map(|m| m.scale_factor()).unwrap_or(1.0);
        let fonts = Fonts::new(&FontConfig {
            family: o.font_family.clone(),
            size_px: font_px(o.font_size, scale),
        })
        .map_err(|e| e.to_string())?;
        let m = fonts.metrics();
        let pad = (o.padding as f64 * scale).round() as f32;
        let size = PhysicalSize::new(
            (o.cols as f32 * m.width + 2.0 * pad) as u32,
            (o.rows as f32 * m.height + 2.0 * pad) as u32,
        );
        let attrs = Window::default_attributes()
            .with_title("Seraphim")
            .with_inner_size(size)
            .with_min_inner_size(PhysicalSize::new(
                (m.width * 10.0) as u32,
                (m.height * 3.0) as u32,
            ));
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        window.set_ime_allowed(true);
        let scale = window.scale_factor();

        let gpu = init_gpu(event_loop, window.clone())?;
        let mut renderer = Renderer::new(&gpu.device, gpu.config.format, fonts, Theme::default());
        if (font_px(o.font_size, scale) - renderer.fonts().size_px()).abs() > 0.1 {
            renderer.set_fonts(self.fonts_for(scale)?);
        }
        let inner = window.inner_size();
        let pad = (o.padding as f64 * scale).round() as f32;
        let (cols, rows) = renderer.grid_size(inner.width, inner.height, pad);

        let bus = EventBus::new();
        let proxy = self.proxy.clone();
        let notify = Notifier(Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Wake);
        }));
        let command = o.command.clone().unwrap_or_else(Command::default_shell);
        let session = Session::spawn(
            1,
            SessionConfig { command, cols, rows, notify: Some(notify), ..Default::default() },
            &bus,
        )
        .map_err(|e| format!("cannot start shell: {e}"))?;

        window.request_redraw();
        Ok(Active {
            window,
            gpu,
            renderer,
            session,
            _bus: bus,
            modifiers: ModifiersState::empty(),
            focused: true,
            scale,
            title: String::new(),
            last_generation: u64::MAX,
        })
    }

    fn fonts_for(&self, scale: f64) -> Result<Fonts, String> {
        Fonts::new(&FontConfig {
            family: self.options.font_family.clone(),
            size_px: font_px(self.options.font_size, scale),
        })
        .map_err(|e| e.to_string())
    }

    fn padding(&self, scale: f64) -> f32 {
        (self.options.padding as f64 * scale).round() as f32
    }
}

fn init_gpu(event_loop: &ActiveEventLoop, window: Arc<Window>) -> Result<Gpu, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(
        Box::new(event_loop.owned_display_handle()),
    ));
    let surface = instance.create_surface(window.clone()).map_err(|e| e.to_string())?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .map_err(|e| format!("no usable GPU adapter: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("seraphim"),
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| e.to_string())?;
    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .ok_or("surface is not supported by this adapter")?;
    // Colors are blended in sRGB space (see shader.wgsl): prefer a non-sRGB
    // format, falling back to whatever the surface offers.
    let caps = surface.get_capabilities(&adapter);
    if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
        config.format = *f;
    }
    config.present_mode = wgpu::PresentMode::AutoVsync;
    surface.configure(&device, &config);
    eprintln!(
        "seraphim: {} ({:?}), surface {:?}",
        adapter.get_info().name,
        adapter.get_info().backend,
        config.format
    );
    Ok(Gpu { surface, device, queue, config })
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(a) => self.active = Some(a),
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: UserEvent) {
        let Some(a) = &self.active else { return };
        if let Some(exit) = a.session.exit_info() {
            self.exit_code = exit.code.unwrap_or(1);
            event_loop.exit();
            return;
        }
        if a.session.generation() != a.last_generation {
            a.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let pad = self.active.as_ref().map(|a| self.padding(a.scale)).unwrap_or(0.0);
        let Some(a) = &mut self.active else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                a.gpu.config.width = size.width.max(1);
                a.gpu.config.height = size.height.max(1);
                a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
                let (cols, rows) = a.renderer.grid_size(size.width, size.height, pad);
                let _ = a.session.resize(cols, rows);
                a.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                a.scale = scale_factor;
                let fonts = Fonts::new(&FontConfig {
                    family: self.options.font_family.clone(),
                    size_px: font_px(self.options.font_size, scale_factor),
                });
                if let Ok(fonts) = fonts {
                    a.renderer.set_fonts(fonts);
                }
                a.window.request_redraw();
            }
            WindowEvent::Focused(focused) => {
                a.focused = focused;
                let bytes = encode_focus(focused, a.session.terminal().modes());
                if !bytes.is_empty() {
                    let _ = a.session.write(&bytes);
                }
                a.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => a.modifiers = m.state(),
            // Synthetic presses are replayed by winit for keys already held
            // when focus arrives; forwarding them would duplicate input.
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => {
                if let Some(bytes) = key_bytes(&event, a.modifiers, &a.session) {
                    let _ = a.session.write(&bytes);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                let _ = a.session.write(text.as_bytes());
            }
            WindowEvent::DroppedFile(path) => {
                // Paste the path, quoted, like most terminals do.
                let quoted = format!("'{}' ", path.display().to_string().replace('\'', "'\\''"));
                let bytes = encode_paste(&quoted, a.session.terminal().modes());
                let _ = a.session.write(&bytes);
            }
            WindowEvent::RedrawRequested => {
                let size = a.window.inner_size();
                let view = View {
                    width: size.width,
                    height: size.height,
                    padding: pad,
                    focused: a.focused,
                    cursor_on: true,
                };
                {
                    let term = a.session.terminal();
                    a.last_generation = a.session.generation();
                    if term.title() != a.title {
                        a.title = term.title().to_owned();
                        let t =
                            if a.title.is_empty() { "Seraphim".into() } else { a.title.clone() };
                        a.window.set_title(&t);
                    }
                    a.renderer.prepare(&a.gpu.device, &a.gpu.queue, &term, view);
                }
                draw(a);
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Dropping the session hangs up the shell (SIGHUP to its group).
        self.active = None;
    }
}

fn draw(a: &mut Active) {
    let frame = match a.gpu.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) => f,
        wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            f
        }
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            a.window.request_redraw();
            return;
        }
        // Timeout / occluded / validation: skip this frame.
        _ => return,
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = a.gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(a.renderer.clear_color()),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        a.renderer.render(&mut pass);
    }
    a.gpu.queue.submit([encoder.finish()]);
    a.window.pre_present_notify();
    a.gpu.queue.present(frame);
}

/// Translate a winit key press into bytes for the application.
fn key_bytes(event: &KeyEvent, mods: ModifiersState, session: &Session) -> Option<Vec<u8>> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let m = Mods { shift: mods.shift_key(), alt: mods.alt_key(), ctrl: mods.control_key() };
    let modes = session.terminal().modes().clone();
    let named = |k: Key| Some(encode_key(k, m, &modes));
    match &event.logical_key {
        WKey::Named(n) => match n {
            NamedKey::Enter => named(Key::Enter),
            NamedKey::Tab => named(Key::Tab),
            NamedKey::Backspace => named(Key::Backspace),
            NamedKey::Escape => named(Key::Escape),
            NamedKey::ArrowUp => named(Key::Up),
            NamedKey::ArrowDown => named(Key::Down),
            NamedKey::ArrowLeft => named(Key::Left),
            NamedKey::ArrowRight => named(Key::Right),
            NamedKey::Home => named(Key::Home),
            NamedKey::End => named(Key::End),
            NamedKey::Insert => named(Key::Insert),
            NamedKey::Delete => named(Key::Delete),
            NamedKey::PageUp => named(Key::PageUp),
            NamedKey::PageDown => named(Key::PageDown),
            NamedKey::Space => named(Key::Char(' ')),
            NamedKey::F1 => named(Key::F(1)),
            NamedKey::F2 => named(Key::F(2)),
            NamedKey::F3 => named(Key::F(3)),
            NamedKey::F4 => named(Key::F(4)),
            NamedKey::F5 => named(Key::F(5)),
            NamedKey::F6 => named(Key::F(6)),
            NamedKey::F7 => named(Key::F(7)),
            NamedKey::F8 => named(Key::F(8)),
            NamedKey::F9 => named(Key::F(9)),
            NamedKey::F10 => named(Key::F(10)),
            NamedKey::F11 => named(Key::F(11)),
            NamedKey::F12 => named(Key::F(12)),
            _ => None,
        },
        WKey::Character(s) if m.ctrl || m.alt => {
            let c = s.chars().next()?;
            Some(encode_key(Key::Char(c), m, &modes))
        }
        // Plain typing: use the composed text (dead keys, compose, layouts).
        _ => event.text.as_ref().map(|t| t.as_bytes().to_vec()),
    }
}
